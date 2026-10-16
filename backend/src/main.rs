use ripress::{
    app::App,
    next::Next,
    req::HttpRequest,
    res::HttpResponse,
    types::{HttpMethods, RouterFns},
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tracing::{error, info, warn};
use uuid::Uuid;

mod nominatim_keywords;
mod scoring;

const CATEGORIES: &[&str] = &[
    "Dental clinics",
    "Restaurants",
    "Gyms",
    "Auto repair",
    "Salons",
    "Pharmacies",
    "Software engineering companies",
    "Veterinary clinics",
    "Hotels",
    "Real estate agencies",
    "Grocery stores",
    "Clothing stores",
    "Law firms",
];

const PHOTON_SUGGESTIONS_URL: &str = "https://photon.komoot.io/api/";
const SUGGESTION_CACHE_TTL_HOURS: i64 = 24;
const SUGGESTION_RATE_LIMIT: usize = 30;
const SUGGESTION_RATE_WINDOW: Duration = Duration::from_secs(60);
const SEARCH_RATE_PER_MINUTE: usize = 2;
const SEARCH_RATE_PER_HOUR: usize = 6;
const SEARCH_RATE_MINUTE_WINDOW: Duration = Duration::from_secs(60);
const SEARCH_RATE_HOUR_WINDOW: Duration = Duration::from_secs(60 * 60);
const RATE_LIMIT_IP_CAPACITY: usize = 10_000;
const MAX_RUNNING_SEARCHES: usize = 3;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchState {
    id: String,
    status: String,
    done: usize,
    total: usize,
    leads: Vec<Lead>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    place: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Lead {
    id: String,
    name: String,
    category: String,
    address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    website: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    email_status: String,
    fetch_status: String,
    score: u8,
    reasons: Vec<serde_json::Value>,
}

#[derive(Deserialize, Serialize)]
struct SearchInput {
    location: String,
    category: String,
}

#[derive(Deserialize, Serialize)]
struct NominatimPlace {
    lat: String,
    lon: String,
    address: Option<HashMap<String, serde_json::Value>>,
    display_name: Option<String>,
    addresstype: Option<String>,
    #[serde(default)]
    place: Option<String>,
    #[serde(rename = "type")]
    place_type: Option<String>,
    place_rank: Option<i32>,
    #[serde(default)]
    boundingbox: Option<[String; 4]>,
}

#[derive(Deserialize)]
struct NominatimBusiness {
    osm_type: Option<String>,
    osm_id: Option<i64>,
    name: Option<String>,
    display_name: Option<String>,
    lat: Option<String>,
    lon: Option<String>,
    address: Option<HashMap<String, serde_json::Value>>,
    extratags: Option<HashMap<String, String>>,
}

#[derive(Clone, Deserialize, Serialize)]
struct PlaceSuggestion {
    label: String,
}

#[derive(Deserialize)]
struct PhotonResponse {
    #[serde(default)]
    features: Vec<PhotonFeature>,
}

#[derive(Deserialize)]
struct PhotonFeature {
    properties: Option<PhotonProperties>,
}

#[derive(Deserialize)]
struct PhotonProperties {
    name: Option<String>,
    city: Option<String>,
    state: Option<String>,
    country: Option<String>,
    #[serde(rename = "type")]
    place_type: Option<String>,
    osm_value: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct OverpassResponse {
    elements: Vec<OsmElement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DataSource {
    Live,
    Nominatim,
    Cache,
    Snapshot,
}

impl DataSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Nominatim => "nominatim",
            Self::Cache => "cache",
            Self::Snapshot => "snapshot",
        }
    }
}

#[derive(Default)]
struct HostPacer {
    next_slot: Mutex<HashMap<String, tokio::time::Instant>>,
}

impl HostPacer {
    async fn wait(&self, url: &reqwest::Url) {
        let Some(host) = url.host_str() else { return };
        let key = host.to_lowercase();
        let now = tokio::time::Instant::now();
        let slot = {
            let mut next_slot = self.next_slot.lock().await;
            let slot = next_slot.get(&key).copied().unwrap_or(now).max(now);
            next_slot.insert(key, slot + Duration::from_secs(1));
            slot
        };
        tokio::time::sleep_until(slot).await;
    }
}

static HOST_PACER: LazyLock<HostPacer> = LazyLock::new(HostPacer::default);
static NOMINATIM_PACER: LazyLock<GlobalPacer> = LazyLock::new(GlobalPacer::default);
static MEMORY_CACHE: LazyLock<Mutex<HashMap<String, (Instant, serde_json::Value)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static OVERPASS_BREAKERS: LazyLock<Mutex<HashMap<String, tokio::time::Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SUGGESTION_REQUESTS: LazyLock<Mutex<HashMap<String, VecDeque<Instant>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SEARCH_REQUESTS: LazyLock<Mutex<HashMap<String, VecDeque<Instant>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static RUNNING_SEARCH_SLOTS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_RUNNING_SEARCHES)));

#[derive(Default)]
struct GlobalPacer {
    next_slot: Mutex<Option<tokio::time::Instant>>,
}

impl GlobalPacer {
    async fn wait(&self) {
        let now = tokio::time::Instant::now();
        let slot = {
            let mut next = self.next_slot.lock().await;
            let slot = next.map_or(now, |scheduled| scheduled.max(now));
            *next = Some(slot + Duration::from_secs(1));
            slot
        };
        tokio::time::sleep_until(slot).await;
    }
}

#[derive(Deserialize, Serialize)]
struct OsmElement {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    tags: HashMap<String, String>,
}

#[tokio::main]
async fn main() {
    if dotenvy::dotenv().is_err() {
        dotenvy::from_path("backend/.env").ok();
    }
    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()))
        .init();

    let pool = if let Ok(url) = env::var("DATABASE_URL") {
        match PgPoolOptions::new().max_connections(3).connect(&url).await {
            Ok(pool) => {
                if let Err(e) = sqlx::query("CREATE TABLE IF NOT EXISTS cache (key text PRIMARY KEY, value jsonb NOT NULL, fetched_at timestamptz NOT NULL DEFAULT now())").execute(&pool).await {
                    warn!("cache table initialization failed: {e}");
                }
                Some(pool)
            }
            Err(e) => {
                warn!("Postgres cache unavailable; continuing without it: {e}");
                None
            }
        }
    } else {
        None
    };
    let searches: Arc<RwLock<HashMap<String, SearchState>>> = Arc::new(RwLock::new(HashMap::new()));
    let allowed_origins = Arc::new(configured_cors_origins());
    let port = env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let mut app = App::new();
    app.use_pre_middleware(
        None,
        move |req: HttpRequest, res: HttpResponse, next: Next| {
            let allowed_origins = Arc::clone(&allowed_origins);
            async move {
                let origin = req.headers.get("origin").map(str::to_owned);
                let allowed_origin = origin
                    .as_deref()
                    .map(normalize_origin)
                    .filter(|origin| allowed_origins.contains(origin));

                if req.method == HttpMethods::OPTIONS {
                    let Some(origin) = allowed_origin else {
                        return (req, Some(res.status(403).text("Origin not allowed")));
                    };
                    let response = res
                        .status(204)
                        .set_header("Access-Control-Allow-Origin", origin)
                        .set_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
                        .set_header("Access-Control-Allow-Headers", "Content-Type")
                        .set_header("Vary", "Origin");
                    return (req, Some(response));
                }

                let res = if let Some(origin) = allowed_origin {
                    res.set_header("Access-Control-Allow-Origin", origin)
                        .set_header("Vary", "Origin")
                } else {
                    res
                };
                next.call(req, res).await
            }
        },
    );

    app.get("/health", |_: HttpRequest, res| async move {
        res.ok().json(serde_json::json!({"ok": true}))
    });

    app.get("/api/categories", |_: HttpRequest, res| async move {
        let categories: Vec<_> = CATEGORIES
            .iter()
            .map(|label| {
                serde_json::json!({
                    "id": label.to_ascii_lowercase().replace(' ', "-"),
                    "label": label,
                })
            })
            .collect();
        res.ok().json(categories)
    });

    let suggest_pool = pool.clone();
    app.get("/api/places/suggest", move |req: HttpRequest, res| {
        let pool = suggest_pool.clone();
        async move {
            let query = req.query.get("q").unwrap_or("").trim();
            if query.chars().count() < 3 || query.chars().count() > 100 {
                return res.ok().json(Vec::<PlaceSuggestion>::new());
            }
            let ip = request_ip(&req);
            if !allow_place_suggestion(&ip).await {
                return res.status(429).json(serde_json::json!({"error":"Too many place suggestions. Please wait a minute before trying again."}));
            }
            let cache_key = format!("photon:suggest:{}", query.to_lowercase());
            if let Some(cached) = cache_get::<Vec<PlaceSuggestion>>(
                pool.as_ref(),
                &cache_key,
                SUGGESTION_CACHE_TTL_HOURS,
            )
            .await
            {
                return res.ok().json(cached);
            }
            let client = match build_http_client() {
                Ok(client) => client,
                Err(error) => {
                    error!(%error, "Photon client configuration failed");
                    return res.status(500).json(serde_json::json!({"error":"Place suggestions are not configured"}));
                }
            };
            let response = match client
                .get(PHOTON_SUGGESTIONS_URL)
                .header(reqwest::header::ACCEPT_LANGUAGE, "en")
                .query(&[("q", query), ("limit", "5")])
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => response,
                Ok(response) => {
                    warn!(status=%response.status(), "Photon place suggestions failed");
                    return res.status(502).json(serde_json::json!({"error":"Place suggestions are temporarily unavailable"}));
                }
                Err(error) => {
                    warn!(%error, "Photon place suggestions request failed");
                    return res.status(502).json(serde_json::json!({"error":"Place suggestions are temporarily unavailable"}));
                }
            };
            let photon = match response.json::<PhotonResponse>().await {
                Ok(response) => response,
                Err(error) => {
                    warn!(%error, "Photon returned invalid place suggestions");
                    return res.status(502).json(serde_json::json!({"error":"Place suggestions are temporarily unavailable"}));
                }
            };
            let suggestions = photon_suggestions(photon);
            cache_put(pool.as_ref(), &cache_key, &suggestions).await;
            res.ok().json(suggestions)
        }
    });

    let post_states = searches.clone();
    let post_pool = pool.clone();
    app.post("/api/searches", move |req: HttpRequest, res| {
        let states = post_states.clone();
        let pool = post_pool.clone();
        async move {
            let ip = request_ip(&req);
            if !allow_search_start(&ip).await {
                return res.status(429).json(serde_json::json!({"error":"Too many searches. Please wait before starting another search."}));
            }
            let input = match req.json::<SearchInput>() {
                Ok(value) => value,
                Err(_) => return res.bad_request().json(serde_json::json!({"error":"Request body must contain location and category"})),
            };
            let location = input.location.trim();
            if location.is_empty() || location.chars().count() > 100 {
                return res.bad_request().json(serde_json::json!({"error":"location must be non-empty and at most 100 characters"}));
            }
            if !CATEGORIES.contains(&input.category.as_str()) {
                    let categories = CATEGORIES.join(", ");
                    return res.bad_request().json(serde_json::json!({"error":format!("category must be one of: {categories}")}));
            }
            let search_permit = match RUNNING_SEARCH_SLOTS.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => return res.status(429).json(serde_json::json!({"error":"Busy, try again shortly"})),
            };
            let location = location.to_owned();
            let client = match build_http_client() {
                Ok(client) => client,
                Err(error) => {
                    error!("HTTP client configuration failed: {error}");
                    return res.status(500).json(serde_json::json!({"error":"Search service is not configured"}));
                }
            };
            let ttl_hours = cache_ttl_hours(&location, &input.category);
            let place = match resolve_place(&client, pool.as_ref(), &location, ttl_hours).await {
                Ok(place) => place,
                Err(GeocodeError::TooBroad) => return res.bad_request().json(serde_json::json!({"error":"Please enter a city or town"})),
                Err(GeocodeError::Unavailable(detail)) => {
                    warn!(%location, detail=%detail, "geocoding request failed");
                    return res.status(503).json(serde_json::json!({"error":"Location lookup is temporarily unavailable. Please try again."}));
                }
            };
            let id = Uuid::new_v4().to_string();
            states.write().await.insert(id.clone(), SearchState {
                id: id.clone(), status: "discovering".into(), done: 0, total: 0, leads: vec![], error: None, source: None, place: place.display_name.clone(), stage: Some("Checking cached results…".into()),
            });
            let search_id = id.clone();
            let category = input.category;
            tokio::spawn(async move {
                let _search_permit = search_permit;
                if let Err(e) = discover_job(search_id.clone(), location, category, place, client, states.clone(), pool).await {
                    error!(search_id, detail=%e, "discovery failed");
                    if let Some(state) = states.write().await.get_mut(&search_id) {
                        state.status = "failed".into(); state.error = Some(FRIENDLY_MAP_ERROR.into()); state.stage = Some("Search failed".into());
                    }
                }
            });
            res.status(202).json(serde_json::json!({"id":id}))
        }
    });

    let get_states = searches.clone();
    app.get("/api/searches/:id", move |req: HttpRequest, res| {
        let states = get_states.clone();
        async move {
            let id = req.params.get("id").unwrap_or("");
            match states.read().await.get(id).cloned() {
                Some(state) => res.ok().json(state),
                None => res
                    .status(404)
                    .json(serde_json::json!({"error":"Search not found"})),
            }
        }
    });

    app.listen(port, || info!(port, "Upside API listening"))
        .await;
}

async fn allow_place_suggestion(ip: &str) -> bool {
    let now = Instant::now();
    let mut requests = SUGGESTION_REQUESTS.lock().await;
    allow_rate_window(
        &mut requests,
        ip,
        now,
        SUGGESTION_RATE_WINDOW,
        SUGGESTION_RATE_LIMIT,
    )
}

async fn allow_search_start(ip: &str) -> bool {
    let now = Instant::now();
    let mut requests = SEARCH_REQUESTS.lock().await;
    if !prune_rate_limit_map(&mut requests, ip, now, SEARCH_RATE_HOUR_WINDOW) {
        return false;
    }
    let history = requests.entry(ip.to_owned()).or_default();
    while history
        .front()
        .is_some_and(|request| now.duration_since(*request) >= SEARCH_RATE_HOUR_WINDOW)
    {
        history.pop_front();
    }
    if !search_rate_allowed(history, now) {
        return false;
    }
    history.push_back(now);
    true
}

fn search_rate_allowed(history: &VecDeque<Instant>, now: Instant) -> bool {
    let requests_in_last_minute = history
        .iter()
        .filter(|request| now.duration_since(**request) < SEARCH_RATE_MINUTE_WINDOW)
        .count();
    history.len() < SEARCH_RATE_PER_HOUR && requests_in_last_minute < SEARCH_RATE_PER_MINUTE
}

fn request_ip(req: &HttpRequest) -> String {
    let parse_ip = |value: &str| value.trim().parse::<IpAddr>().ok();
    req.headers
        .x_forwarded_for()
        // Reverse proxies append the connecting client address to this list.
        // Taking the rightmost valid IP avoids trusting client-supplied prefixes.
        .and_then(|value| value.split(',').rev().find_map(parse_ip))
        .or_else(|| req.headers.get("x-real-ip").and_then(parse_ip))
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn allow_rate_window(
    requests: &mut HashMap<String, VecDeque<Instant>>,
    ip: &str,
    now: Instant,
    window: Duration,
    limit: usize,
) -> bool {
    if !prune_rate_limit_map(requests, ip, now, window) {
        return false;
    }
    let history = requests.entry(ip.to_owned()).or_default();
    while history
        .front()
        .is_some_and(|request| now.duration_since(*request) >= window)
    {
        history.pop_front();
    }
    if history.len() >= limit {
        return false;
    }
    history.push_back(now);
    true
}

fn prune_rate_limit_map(
    requests: &mut HashMap<String, VecDeque<Instant>>,
    ip: &str,
    now: Instant,
    retention: Duration,
) -> bool {
    if requests.len() >= RATE_LIMIT_IP_CAPACITY && !requests.contains_key(ip) {
        requests.retain(|_, history| {
            history
                .back()
                .is_some_and(|last_request| now.duration_since(*last_request) < retention)
        });
        if requests.len() >= RATE_LIMIT_IP_CAPACITY {
            return false;
        }
    }
    true
}

fn configured_cors_origins() -> HashSet<String> {
    parse_cors_origins(
        &env::var("ALLOWED_ORIGIN").unwrap_or_else(|_| "http://localhost:5173".to_owned()),
    )
}

fn parse_cors_origins(value: &str) -> HashSet<String> {
    value
        .split(',')
        .map(normalize_origin)
        .filter(|origin| !origin.is_empty() && origin != "*")
        .collect()
}

fn normalize_origin(origin: &str) -> String {
    origin.trim().trim_end_matches('/').to_owned()
}

fn photon_suggestions(response: PhotonResponse) -> Vec<PlaceSuggestion> {
    let mut suggestions = Vec::new();
    for feature in response.features {
        let Some(properties) = feature.properties else {
            continue;
        };
        let kind = properties
            .place_type
            .as_deref()
            .or(properties.osm_value.as_deref())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(kind.as_str(), "city" | "town" | "village") {
            continue;
        }
        let name = properties.city.or(properties.name).unwrap_or_default();
        if name.trim().is_empty() {
            continue;
        }
        let mut parts = vec![name.trim().to_owned()];
        for part in [properties.state, properties.country].into_iter().flatten() {
            let part = part.trim();
            if !part.is_empty()
                && !parts
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(part))
            {
                parts.push(part.to_owned());
            }
        }
        let label = parts.join(", ");
        if !suggestions
            .iter()
            .any(|suggestion: &PlaceSuggestion| suggestion.label == label)
        {
            suggestions.push(PlaceSuggestion { label });
        }
        if suggestions.len() == 5 {
            break;
        }
    }
    suggestions
}

const FRIENDLY_MAP_ERROR: &str = "The public map servers are busy. Try again in a minute, or a saved sample is shown if available.";

fn build_http_client() -> Result<reqwest::Client, String> {
    let mailto = env::var("UPSIDE_CONTACT")
        .ok()
        .filter(|address| {
            address.contains('@')
                && !address.ends_with("@example.com")
                && !address.contains(char::is_whitespace)
        })
        .ok_or_else(|| "UPSIDE_CONTACT is not configured".to_string())?;
    let force_ipv4 = env::var("OVERPASS_FORCE_IPV4")
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(true);
    let mut builder = reqwest::Client::builder()
        .user_agent(format!("UpsideBot/0.1 (+mailto:{mailto})"))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(45));
    if force_ipv4 {
        builder = builder.local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }
    builder.build().map_err(|error| error.to_string())
}

enum GeocodeError {
    TooBroad,
    Unavailable(String),
}

fn is_city_or_town(place: &NominatimPlace) -> bool {
    let city_type = |kind: &str| {
        matches!(
            kind.to_ascii_lowercase().as_str(),
            "city" | "town" | "village"
        )
    };
    let address_place_type = place
        .address
        .as_ref()
        .and_then(|address| address.get("place"))
        .and_then(serde_json::Value::as_str);
    if place.place.as_deref().is_some_and(city_type)
        || place.place_type.as_deref().is_some_and(city_type)
        || place.addresstype.as_deref().is_some_and(city_type)
        || address_place_type.is_some_and(city_type)
    {
        return true;
    }
    let broad_type = place.addresstype.as_deref().is_some_and(|kind| {
        matches!(
            kind.to_ascii_lowercase().as_str(),
            "country" | "state" | "region" | "province" | "county" | "administrative"
        )
    });
    let rank_too_broad = place.place_rank.is_some_and(|rank| rank <= 12);
    !broad_type && !rank_too_broad
}

async fn resolve_place(
    client: &reqwest::Client,
    pool: Option<&PgPool>,
    location: &str,
    ttl_hours: i64,
) -> Result<NominatimPlace, GeocodeError> {
    let key = format!("geocode:v3:{}", location.to_lowercase());
    if let Some(place) = cache_get::<NominatimPlace>(pool, &key, ttl_hours).await {
        return if is_city_or_town(&place) {
            Ok(place)
        } else {
            Err(GeocodeError::TooBroad)
        };
    }
    let nominatim_url = env::var("NOMINATIM_URL")
        .unwrap_or_else(|_| "https://nominatim.openstreetmap.org/search".into());
    NOMINATIM_PACER.wait().await;
    let mut request = client
        .get(nominatim_url)
        .header(reqwest::header::ACCEPT_LANGUAGE, "en")
        .query(&[
            ("q", location),
            ("format", "json"),
            ("limit", "1"),
            ("addressdetails", "1"),
            (
                "email",
                env::var("UPSIDE_CONTACT").unwrap_or_default().as_str(),
            ),
        ]);
    if let Ok(referer) = env::var("UPSIDE_REFERER") {
        request = request.header(reqwest::header::REFERER, referer);
    }
    let response = request
        .send()
        .await
        .map_err(|error| GeocodeError::Unavailable(error.to_string()))?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        return Err(GeocodeError::Unavailable(format!(
            "Nominatim HTTP {status}: {}",
            first_chars(&detail, 300)
        )));
    }
    let places: Vec<NominatimPlace> = response
        .json()
        .await
        .map_err(|error| GeocodeError::Unavailable(error.to_string()))?;
    let place = places
        .into_iter()
        .next()
        .ok_or_else(|| GeocodeError::TooBroad)?;
    if !is_city_or_town(&place) {
        return Err(GeocodeError::TooBroad);
    }
    cache_put(pool, &key, &place).await;
    Ok(place)
}

fn cache_ttl_hours(location: &str, category: &str) -> i64 {
    if demo_snapshot_name(location, category).is_some() {
        return 168;
    }
    env::var("CACHE_TTL_HOURS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|hours| *hours > 0)
        .unwrap_or(24)
}

fn elements_to_leads(
    elements: Vec<OsmElement>,
    category: &str,
    cfg: &scoring::ScoringConfig,
    default_id_prefix: &str,
) -> Vec<Lead> {
    let mut seen = std::collections::HashSet::new();
    let mut leads: Vec<Lead> = elements
        .into_iter()
        .filter_map(|element| {
            let name = element.tags.get("name")?.trim();
            if name.is_empty() || !seen.insert(name.to_lowercase()) {
                return None;
            }
            let address = element.tags.get("_address").cloned().unwrap_or_else(|| {
                [
                    "addr:housenumber",
                    "addr:street",
                    "addr:city",
                    "addr:postcode",
                ]
                .iter()
                .filter_map(|key| element.tags.get(*key))
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
            });
            let website = element
                .tags
                .get("website")
                .or_else(|| element.tags.get("contact:website"))
                .cloned();
            let phone = element
                .tags
                .get("phone")
                .or_else(|| element.tags.get("contact:phone"))
                .cloned();
            let email = element
                .tags
                .get("email")
                .or_else(|| element.tags.get("contact:email"))
                .cloned();
            let has_website = website
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
            let affiliated = scoring::is_brand_affiliated(category, name, &element.tags, cfg);
            let (score, reasons) = scoring::listing_score(has_website, affiliated, cfg);
            Some(Lead {
                id: element
                    .tags
                    .get("_lead_id")
                    .cloned()
                    .unwrap_or_else(|| format!("{default_id_prefix}{}", element.id)),
                name: name.into(),
                category: category.into(),
                address,
                website,
                phone,
                email,
                email_status: "none".into(),
                fetch_status: "no_website".into(),
                score,
                reasons,
            })
        })
        .collect();
    leads.sort_by_key(|lead| scoring::ordering_key(lead.score, lead.phone.is_some(), &lead.name));
    leads.truncate(40);
    leads
}

async fn get_nominatim_businesses(
    client: &reqwest::Client,
    pool: Option<&PgPool>,
    search_id: &str,
    category: &str,
    place: &NominatimPlace,
    ttl_hours: i64,
) -> Result<(OverpassResponse, DataSource), String> {
    let bounds = place
        .boundingbox
        .as_ref()
        .ok_or_else(|| "geocoder did not return a bounding box".to_string())?;
    let south: f64 = bounds[0]
        .parse()
        .map_err(|_| "invalid Nominatim south bound")?;
    let north: f64 = bounds[1]
        .parse()
        .map_err(|_| "invalid Nominatim north bound")?;
    let west: f64 = bounds[2]
        .parse()
        .map_err(|_| "invalid Nominatim west bound")?;
    let east: f64 = bounds[3]
        .parse()
        .map_err(|_| "invalid Nominatim east bound")?;
    let viewbox = format!("{west},{north},{east},{south}");
    let cache_key = nominatim_cache_key(category, place);
    if let Some(data) = cache_get::<OverpassResponse>(pool, &cache_key, ttl_hours).await {
        info!(
            search_id,
            category,
            source = "cache",
            businesses = data.elements.len(),
            "using cached Nominatim results"
        );
        return Ok((data, DataSource::Cache));
    }
    let contact =
        env::var("UPSIDE_CONTACT").map_err(|_| "UPSIDE_CONTACT is not configured".to_string())?;
    let endpoint = env::var("NOMINATIM_URL")
        .unwrap_or_else(|_| "https://nominatim.openstreetmap.org/search".into());
    let keyword = nominatim_keywords::keyword(category);
    let mut queries = Vec::new();
    if let Some(keyword) = keyword {
        queries.push((format!("[{keyword}]"), "configured_keyword"));
    }
    let display = place.display_name.as_deref().unwrap_or("");
    let fallback_query = format!("{} in {}", category.to_lowercase(), display);
    if keyword.is_none() {
        info!(
            search_id,
            category, "no configured Nominatim keyword; using category and place query"
        );
    }
    queries.push((fallback_query, "category_place_fallback"));
    for (query, query_kind) in queries {
        NOMINATIM_PACER.wait().await;
        let mut request = client
            .get(&endpoint)
            .header(reqwest::header::ACCEPT_LANGUAGE, "en")
            .query(&[
                ("q", query.as_str()),
                ("viewbox", viewbox.as_str()),
                ("bounded", "1"),
                ("limit", "40"),
                ("format", "jsonv2"),
                ("extratags", "1"),
                ("addressdetails", "1"),
                ("email", contact.as_str()),
            ])
            .timeout(Duration::from_secs(12));
        if let Ok(referer) = env::var("UPSIDE_REFERER") {
            request = request.header(reqwest::header::REFERER, referer);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => return Err(format!("Nominatim {query_kind} request: {error}")),
        };
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if query_kind == "configured_keyword" && status == reqwest::StatusCode::BAD_REQUEST {
                info!(search_id, category, detail=%first_chars(&body, 200), "configured Nominatim keyword was not accepted; trying category/place query");
                continue;
            }
            return Err(format!(
                "Nominatim {query_kind} HTTP {status}: {}",
                first_chars(&body, 300)
            ));
        }
        let results: Vec<NominatimBusiness> = response
            .json()
            .await
            .map_err(|error| format!("Nominatim response: {error}"))?;
        let mut elements = Vec::with_capacity(results.len());
        for result in results {
            let Some(name) = result
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            let mut tags = HashMap::new();
            tags.insert("name".into(), name.to_owned());
            if let Some(extra) = result.extratags {
                tags.extend(extra);
            }
            if let Some(address) = result.address {
                for (key, value) in address {
                    if let Some(value) = value.as_str() {
                        let osm_key = match key.as_str() {
                            "house_number" => Some("addr:housenumber"),
                            "road" | "pedestrian" | "residential" => Some("addr:street"),
                            "postcode" => Some("addr:postcode"),
                            "city" | "town" | "village" | "municipality" => Some("addr:city"),
                            _ => None,
                        };
                        if let Some(osm_key) = osm_key {
                            tags.insert(osm_key.into(), value.into());
                        }
                    }
                }
            }
            let address = result
                .display_name
                .clone()
                .or_else(|| {
                    [
                        "addr:housenumber",
                        "addr:street",
                        "addr:city",
                        "addr:postcode",
                    ]
                    .iter()
                    .filter_map(|key| tags.get(*key))
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .reduce(|a, b| format!("{a}, {b}"))
                })
                .or_else(|| {
                    Some(format!(
                        "{}, {}",
                        result.lat.as_deref().unwrap_or(""),
                        result.lon.as_deref().unwrap_or("")
                    ))
                });
            if let Some(address) = address {
                tags.insert("_address".into(), address);
            }
            let osm_id = result.osm_id.unwrap_or(0);
            tags.insert(
                "_lead_id".into(),
                format!(
                    "nominatim-{}-{osm_id}",
                    result.osm_type.as_deref().unwrap_or("osm")
                ),
            );
            elements.push(OsmElement { id: osm_id, tags });
        }
        info!(
            search_id,
            category,
            query_kind,
            result_count = elements.len(),
            "Nominatim fallback query"
        );
        if !elements.is_empty() {
            let data = OverpassResponse { elements };
            cache_put(pool, &cache_key, &data).await;
            return Ok((data, DataSource::Nominatim));
        }
        if query_kind == "configured_keyword" {
            info!(
                search_id,
                category,
                "configured Nominatim keyword returned no named results; trying category/place query"
            );
        }
    }
    let empty = OverpassResponse { elements: vec![] };
    cache_put(pool, &cache_key, &empty).await;
    Ok((empty, DataSource::Nominatim))
}

fn nominatim_cache_key(category: &str, place: &NominatimPlace) -> String {
    format!(
        "nominatim:{}:{:.2}:{:.2}",
        category,
        place.lat.parse::<f64>().unwrap_or_default(),
        place.lon.parse::<f64>().unwrap_or_default()
    )
}

async fn discover_job(
    id: String,
    location: String,
    category: String,
    place: NominatimPlace,
    client: reqwest::Client,
    states: Arc<RwLock<HashMap<String, SearchState>>>,
    pool: Option<PgPool>,
) -> Result<(), String> {
    let discovery_started = Instant::now();
    info!(search_id=%id, %location, %category, "starting discovery");
    if let Some(state) = states.write().await.get_mut(&id) {
        state.stage = Some("Checking cached results…".into());
    }
    let ttl_hours = cache_ttl_hours(&location, &category);
    let geocode_started = Instant::now();
    info!(search_id=%id, stage="geocode", elapsed_ms=geocode_started.elapsed().as_millis() as u64, place=%place.display_name.as_deref().unwrap_or(""));
    let latitude: f64 = place
        .lat
        .parse()
        .map_err(|_| "Nominatim returned invalid latitude".to_string())?;
    let longitude: f64 = place
        .lon
        .parse()
        .map_err(|_| "Nominatim returned invalid longitude".to_string())?;
    let country_code = place
        .address
        .as_ref()
        .and_then(|a| a.get("country_code"))
        .and_then(|v| v.as_str())
        .unwrap_or("us")
        .to_owned();
    let tag = match category.as_str() {
        "Dental clinics" => "amenity=dentist",
        "Restaurants" => "amenity=restaurant",
        "Gyms" => "leisure=fitness_centre",
        "Auto repair" => "shop=car_repair",
        "Salons" => "shop=hairdresser",
        "Pharmacies" => "amenity=pharmacy",
        "Software engineering companies" => "office=it",
        "Veterinary clinics" => "amenity=veterinary",
        "Hotels" => "tourism=hotel",
        "Real estate agencies" => "office=estate_agent",
        "Grocery stores" => "shop=supermarket",
        "Clothing stores" => "shop=clothes",
        "Law firms" => "office=lawyer",
        _ => return Err("Unsupported category".into()),
    };
    let cfg = scoring::config();
    let discovery_deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    let overpass_started = Instant::now();
    let mut selected: Option<(OverpassResponse, DataSource)> = None;
    for radius in [6000_u32, 12000_u32] {
        let key = overpass_cache_key(&category, latitude, longitude, radius);
        if let Some(data) = cache_get::<OverpassResponse>(pool.as_ref(), &key, ttl_hours).await {
            info!(search_id=%id, radius, source="cache", businesses=data.elements.len(), "using cached Overpass results");
            selected = Some((data, DataSource::Cache));
            break;
        }
    }
    let nominatim_key = nominatim_cache_key(&category, &place);
    if selected.is_none() {
        if let Some(data) =
            cache_get::<OverpassResponse>(pool.as_ref(), &nominatim_key, ttl_hours).await
        {
            if !data.elements.is_empty() {
                info!(search_id=%id, category, source="cache", businesses=data.elements.len(), "using cached Nominatim results before live discovery");
                selected = Some((data, DataSource::Cache));
            } else {
                info!(search_id=%id, category, "cached Nominatim result was empty; trying Overpass");
            }
        }
    }
    if selected.is_none() {
        if let Some(state) = states.write().await.get_mut(&id) {
            state.stage = Some("Searching OpenStreetMap…".into());
        }
        match get_overpass(
            pool.as_ref(),
            &client,
            &id,
            &category,
            tag,
            latitude,
            longitude,
            12000,
            ttl_hours,
            discovery_deadline,
        )
        .await
        {
            Ok((data, source)) => {
                info!(search_id=%id, source=source.as_str(), elapsed_ms=overpass_started.elapsed().as_millis() as u64, businesses=data.elements.len(), "Overpass discovery complete");
                if source == DataSource::Live {
                    save_demo_snapshot(&location, &category, &id, &data).await;
                }
                selected = Some((data, source));
            }
            Err(detail) => {
                warn!(search_id=%id, detail=%detail, "Overpass unavailable; trying Nominatim fallback");
                if let Some(state) = states.write().await.get_mut(&id) {
                    state.stage = Some("Searching OpenStreetMap backup index…".into());
                }
                match get_nominatim_businesses(
                    &client,
                    pool.as_ref(),
                    &id,
                    &category,
                    &place,
                    ttl_hours,
                )
                .await
                {
                    Ok((data, source)) if !data.elements.is_empty() => {
                        selected = Some((data, source))
                    }
                    Ok(_) => {
                        warn!(search_id=%id, category, "Nominatim fallback returned no named businesses")
                    }
                    Err(detail) => {
                        warn!(search_id=%id, detail=%detail, "Nominatim fallback failed")
                    }
                }
                if selected.is_none() {
                    if let Some(snapshot) = load_demo_snapshot(&location, &category).await {
                        warn!(search_id=%id, location, category, "live providers unavailable; using offline demo snapshot");
                        selected = Some((snapshot, DataSource::Snapshot));
                    }
                }
            }
        }
    }
    let (overpass, source) =
        selected.ok_or_else(|| "All public discovery providers failed".to_string())?;
    let mut leads = elements_to_leads(overpass.elements, &category, &cfg, "osm-");
    let total = leads.len();
    let discovered = total;
    if let Some(state) = states.write().await.get_mut(&id) {
        state.source = Some(source.as_str().to_string());
        state.status = "enriching".into();
        state.done = 0;
        state.total = total;
        state.leads.clear();
        state.stage = Some("Checking websites and scoring leads…".into());
    }
    let enrich_started = Instant::now();
    let metrics = enrich_leads(&mut leads, &client, &cfg, &id, states.clone()).await?;
    info!(search_id=%id, stage="enrichment", elapsed_ms=enrich_started.elapsed().as_millis() as u64);
    leads.sort_by_key(|lead| scoring::ordering_key(lead.score, lead.phone.is_some(), &lead.name));
    info!(search_id=%id, discovered, with_website=metrics.with_website, fetched_ok=metrics.fetched_ok,
        blocked_robots=metrics.blocked, unreachable=metrics.unreachable, emails_found=metrics.emails_found,
        emails_valid=metrics.emails_valid, emails_invalid=metrics.emails_invalid,
        emails_unverified=metrics.emails_unverified, "search stage counts");
    info!(search_id=%id, stage="discovery_total", elapsed_ms=discovery_started.elapsed().as_millis() as u64);
    info!(search_id=%id, %total, country_code, "discovery complete");
    if let Some(state) = states.write().await.get_mut(&id) {
        state.total = total;
        state.leads = leads;
        state.status = "done".into();
        state.source = Some(source.as_str().to_string());
        state.stage = Some("Search complete.".into());
    }
    Ok(())
}

fn overpass_mirrors() -> Vec<String> {
    env::var("OVERPASS_URLS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(str::to_string)
                .collect()
        })
        .filter(|urls: &Vec<String>| !urls.is_empty())
        .unwrap_or_else(|| {
            vec![
                "https://overpass-api.de/api/interpreter".into(),
                "https://lz4.overpass-api.de/api/interpreter".into(),
            ]
        })
}

fn overpass_query(tag: &str, radius: u32, lat: f64, lon: f64) -> String {
    let (key, value) = tag
        .split_once('=')
        .expect("category Overpass tag must be key=value");
    // A small bounding-box lookup can use Overpass's spatial index more
    // efficiently than an `around` filter for dense city centers.
    let lat_delta = radius as f64 / 111_320.0;
    let lon_delta = radius as f64 / (111_320.0 * lat.to_radians().cos().abs().max(0.1));
    let south = lat - lat_delta;
    let west = lon - lon_delta;
    let north = lat + lat_delta;
    let east = lon + lon_delta;
    format!(
        "[out:json][timeout:11];nwr[\"{key}\"=\"{value}\"][\"name\"]({south:.6},{west:.6},{north:.6},{east:.6});out center tags qt 60;"
    )
}

fn overpass_cache_key(category: &str, lat: f64, lon: f64, radius: u32) -> String {
    let lat = (lat * 100.0).round() / 100.0;
    let lon = (lon * 100.0).round() / 100.0;
    format!("overpass:{category}:{lat:.2}:{lon:.2}:{radius}")
}

async fn get_overpass(
    pool: Option<&PgPool>,
    client: &reqwest::Client,
    search_id: &str,
    category: &str,
    tag: &str,
    lat: f64,
    lon: f64,
    radius: u32,
    ttl_hours: i64,
    deadline: tokio::time::Instant,
) -> Result<(OverpassResponse, DataSource), String> {
    let cache_key = overpass_cache_key(category, lat, lon, radius);
    let cache_started = Instant::now();
    if let Some(data) = cache_get::<OverpassResponse>(pool, &cache_key, ttl_hours).await {
        info!(
            search_id,
            category,
            radius,
            cache = "hit",
            stage = "overpass_cache",
            elapsed_ms = cache_started.elapsed().as_millis() as u64,
            businesses = data.elements.len()
        );
        return Ok((data, DataSource::Cache));
    }
    info!(
        search_id,
        category,
        radius,
        cache = "miss",
        stage = "overpass_cache",
        elapsed_ms = cache_started.elapsed().as_millis() as u64
    );
    let query = overpass_query(tag, radius, lat, lon);
    let mirrors: Vec<String> = overpass_mirrors().into_iter().take(2).collect();
    let mut last_error = String::from("no mirrors configured");
    let mut all_breakers_open = !mirrors.is_empty();
    for endpoint in &mirrors {
        if !breaker_is_open(endpoint).await {
            all_breakers_open = false;
            break;
        }
    }
    if all_breakers_open {
        for alternate_radius in [6000_u32, 12000_u32] {
            if alternate_radius == radius {
                continue;
            }
            let alternate_key = overpass_cache_key(category, lat, lon, alternate_radius);
            if let Some(data) = cache_get::<OverpassResponse>(pool, &alternate_key, ttl_hours).await
            {
                info!(
                    search_id,
                    category,
                    radius = alternate_radius,
                    cache = "hit",
                    source = "cache",
                    stage = "overpass_cache",
                    businesses = data.elements.len(),
                    "using alternate-radius cache while all Overpass breakers are open"
                );
                return Ok((data, DataSource::Cache));
            }
        }
    }

    for endpoint in &mirrors {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        if breaker_is_open(endpoint).await {
            warn!(
                search_id,
                endpoint,
                error_type = "circuit_breaker_open",
                "skipping Overpass mirror"
            );
            continue;
        }
        let mirror_started = Instant::now();
        let mirror_url = match reqwest::Url::parse(endpoint) {
            Ok(url) => url,
            Err(error) => {
                open_breaker(endpoint).await;
                let detail = error.to_string();
                warn!(search_id, endpoint, error_type="invalid_url", http_status=?Option::<u16>::None, response_body="", detail=%detail, elapsed_ms=mirror_started.elapsed().as_millis() as u64, "Overpass mirror failed");
                last_error = format!("{endpoint}: invalid URL: {detail}");
                continue;
            }
        };
        if tokio::time::timeout_at(deadline, HOST_PACER.wait(&mirror_url))
            .await
            .is_err()
        {
            break;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let request_timeout = remaining.min(Duration::from_secs(12));
        let attempt = tokio::time::timeout(request_timeout, async {
            let response = client
                .post(endpoint)
                .timeout(request_timeout)
                .form(&[("data", query.as_str())])
                .send()
                .await?;
            let status = response.status();
            let body = response.text().await;
            Ok::<_, reqwest::Error>((status, body))
        })
        .await;

        let failure = match attempt {
            Ok(Ok((status, Ok(body)))) if status.is_success() => {
                match serde_json::from_str::<OverpassResponse>(&body) {
                    Ok(data) => {
                        info!(
                            search_id,
                            endpoint,
                            elapsed_ms = mirror_started.elapsed().as_millis() as u64,
                            outcome = "ok",
                            radius,
                            businesses = data.elements.len(),
                            "Overpass mirror"
                        );
                        clear_breaker(endpoint).await;
                        cache_put(pool, &cache_key, &data).await;
                        return Ok((data, DataSource::Live));
                    }
                    Err(error) => MirrorFailure {
                        error_type: "invalid_payload",
                        http_status: Some(status.as_u16()),
                        body: body.clone(),
                        detail: error.to_string(),
                    },
                }
            }
            Ok(Ok((status, Ok(body)))) => MirrorFailure {
                error_type: "http_status",
                http_status: Some(status.as_u16()),
                body,
                detail: status.to_string(),
            },
            Ok(Ok((status, Err(error)))) => MirrorFailure {
                error_type: classify_http_error(&error),
                http_status: Some(status.as_u16()),
                body: String::new(),
                detail: error.to_string(),
            },
            Ok(Err(error)) => MirrorFailure {
                error_type: classify_http_error(&error),
                http_status: None,
                body: String::new(),
                detail: error.to_string(),
            },
            Err(_) => MirrorFailure {
                error_type: "timeout",
                http_status: None,
                body: String::new(),
                detail: format!("mirror exceeded {}ms timeout", request_timeout.as_millis()),
            },
        };
        open_breaker(endpoint).await;
        let body_preview = first_chars(&failure.body, 300);
        warn!(search_id, endpoint, error_type=failure.error_type, http_status=?failure.http_status, response_body=%body_preview, detail=%failure.detail, elapsed_ms=mirror_started.elapsed().as_millis() as u64, breaker_seconds=300, "Overpass mirror failed");
        last_error = format!(
            "{endpoint}: {}{}{}",
            failure.error_type,
            failure
                .http_status
                .map(|status| format!(" HTTP {status}"))
                .unwrap_or_default(),
            if failure.detail.is_empty() {
                String::new()
            } else {
                format!(": {}", failure.detail)
            }
        );
    }
    Err(format!(
        "All {} Overpass mirrors failed; last failure: {last_error}. Check OVERPASS_URLS or retry when a public instance is available.",
        mirrors.len()
    ))
}

struct MirrorFailure {
    error_type: &'static str,
    http_status: Option<u16>,
    body: String,
    detail: String,
}

fn first_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn classify_http_error(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect_error"
    } else if error.is_decode() {
        "decode_error"
    } else {
        "request_error"
    }
}

async fn breaker_is_open(endpoint: &str) -> bool {
    OVERPASS_BREAKERS
        .lock()
        .await
        .get(endpoint)
        .is_some_and(|until| *until > tokio::time::Instant::now())
}

async fn open_breaker(endpoint: &str) {
    OVERPASS_BREAKERS.lock().await.insert(
        endpoint.to_string(),
        tokio::time::Instant::now() + Duration::from_secs(300),
    );
}

async fn clear_breaker(endpoint: &str) {
    OVERPASS_BREAKERS.lock().await.remove(endpoint);
}

fn demo_snapshot_name(location: &str, category: &str) -> Option<&'static str> {
    let loc = location.to_lowercase();
    match (category, loc.contains("jaipur"), loc.contains("berlin")) {
        ("Auto repair", true, _) => Some("auto_repair_jaipur.json"),
        ("Restaurants", _, true) => Some("restaurants_berlin.json"),
        _ => None,
    }
}

fn demo_snapshot_paths(file: &str) -> Vec<std::path::PathBuf> {
    if let Some(directory) = env::var_os("DEMO_DATA_DIR") {
        return vec![std::path::PathBuf::from(directory).join(file)];
    }
    if std::path::Path::new("backend/Cargo.toml").exists() {
        vec![std::path::PathBuf::from("demo_data").join(file)]
    } else if std::path::Path::new("Cargo.toml").exists() {
        vec![std::path::PathBuf::from("../demo_data").join(file)]
    } else {
        vec![std::path::PathBuf::from("demo_data").join(file)]
    }
}

async fn save_demo_snapshot(
    location: &str,
    category: &str,
    _search_id: &str,
    data: &OverpassResponse,
) {
    let Some(file) = demo_snapshot_name(location, category) else {
        return;
    };
    let encoded = match serde_json::to_vec_pretty(data) {
        Ok(value) => value,
        Err(_) => return,
    };
    for path in demo_snapshot_paths(file) {
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if tokio::fs::write(path, &encoded).await.is_ok() {
            info!(
                search_id = _search_id,
                location, category, "saved Overpass demo snapshot"
            );
            break;
        }
    }
}

async fn load_demo_snapshot(location: &str, category: &str) -> Option<OverpassResponse> {
    let file = demo_snapshot_name(location, category)?;
    for path in demo_snapshot_paths(file) {
        if let Ok(bytes) = tokio::fs::read(path).await {
            if let Ok(data) = serde_json::from_slice(&bytes) {
                return Some(data);
            }
        }
    }
    None
}

#[derive(Default)]
struct EnrichmentCounts {
    with_website: usize,
    fetched_ok: usize,
    blocked: usize,
    unreachable: usize,
    emails_found: usize,
    emails_valid: usize,
    emails_invalid: usize,
    emails_unverified: usize,
}

impl EnrichmentCounts {
    fn add(&mut self, other: Self) {
        self.with_website += other.with_website;
        self.fetched_ok += other.fetched_ok;
        self.blocked += other.blocked;
        self.unreachable += other.unreachable;
        self.emails_found += other.emails_found;
        self.emails_valid += other.emails_valid;
        self.emails_invalid += other.emails_invalid;
        self.emails_unverified += other.emails_unverified;
    }
}

async fn enrich_leads(
    leads: &mut Vec<Lead>,
    client: &reqwest::Client,
    cfg: &scoring::ScoringConfig,
    search_id: &str,
    states: Arc<RwLock<HashMap<String, SearchState>>>,
) -> Result<EnrichmentCounts, String> {
    let mut counts = EnrichmentCounts::default();
    let total = leads.len();
    let cfg = Arc::new(cfg.clone());
    let semaphore = Arc::new(Semaphore::new(12));
    let mut workers = tokio::task::JoinSet::new();
    for (index, lead) in leads.iter().cloned().enumerate() {
        let client = client.clone();
        let cfg = cfg.clone();
        let semaphore = semaphore.clone();
        let search_id = search_id.to_string();
        workers.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((index, enrich_one(lead, &client, &cfg, &search_id).await))
        });
    }

    let mut completed = vec![None; total];
    let mut streamed = Vec::with_capacity(total);
    let mut done = 0;
    while let Some(result) = workers.join_next().await {
        let (index, (lead, lead_counts)) =
            result.map_err(|error| format!("Enrichment worker failed: {error}"))??;
        completed[index] = Some(lead.clone());
        streamed.push(lead);
        counts.add(lead_counts);
        done += 1;
        if let Some(state) = states.write().await.get_mut(search_id) {
            state.done = done;
            state.leads = streamed.clone();
        }
    }
    *leads = completed
        .into_iter()
        .map(|lead| lead.ok_or_else(|| "An enrichment worker returned no lead".to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(counts)
}

async fn enrich_one(
    mut lead: Lead,
    client: &reqwest::Client,
    cfg: &scoring::ScoringConfig,
    search_id: &str,
) -> (Lead, EnrichmentCounts) {
    let mut counts = EnrichmentCounts::default();
    let had_email = lead.email.is_some();
    if had_email {
        counts.emails_found += 1;
        lead.email_status = validate_email_logged(
            client,
            lead.email.as_deref().unwrap(),
            search_id,
            &lead.name,
        )
        .await;
        count_email_status(&mut counts, &lead.email_status);
    }

    let Some(raw_url) = lead.website.as_deref().filter(|url| !url.trim().is_empty()) else {
        let mut reasons = vec![(
            "No website listed in OpenStreetMap (unverified)".to_string(),
            cfg.signals.no_website,
        )];
        if lead
            .reasons
            .iter()
            .any(|r| r["label"] == "Brand-authorised or chain location")
        {
            reasons.push((
                "Brand-authorised or chain location".to_string(),
                cfg.signals.brand_affiliated,
            ));
        }
        (lead.score, lead.reasons) = scoring::score(reasons);
        lead.fetch_status = "no_website".into();
        return (lead, counts);
    };
    counts.with_website += 1;
    let url = if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
        raw_url.to_string()
    } else {
        format!("https://{raw_url}")
    };
    let homepage = match reqwest::Url::parse(&url) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url,
        _ => {
            lead.fetch_status = "unreachable".into();
            counts.unreachable += 1;
            return (lead, counts);
        }
    };

    let mut robots = load_robots_policy(&homepage, search_id, &lead.name).await;
    if robots.disallows(&homepage) {
        lead.fetch_status = "blocked".into();
        counts.blocked += 1;
        info!(search_id, business=%lead.name, host=homepage.host_str().unwrap_or(""), stage="homepage", elapsed_ms=0, outcome="blocked_by_robots", "site request complete");
        return (lead, counts);
    }

    let Some(home_page) = fetch_site_page(&homepage, "homepage", search_id, &lead.name).await
    else {
        lead.fetch_status = "unreachable".into();
        counts.unreachable += 1;
        return (lead, counts);
    };
    let homepage = home_page.final_url;
    let mut homepage = homepage;
    let mut home_html = home_page.html;
    if homepage.scheme() == "https" {
        lead.website = Some(homepage.to_string());
    }
    let mut https_available = homepage.scheme() == "https";
    let mut https_checked = false;
    if !https_available {
        let mut https_url = homepage.clone();
        if https_url.set_scheme("https").is_ok() {
            // An explicit port 80 belongs to the HTTP URL; HTTPS should use its
            // normal port unless the listing specified a separate custom port.
            if homepage.port() == Some(80) {
                let _ = https_url.set_port(None);
            }
            let https_robots = load_robots_policy(&https_url, search_id, &lead.name).await;
            if !https_robots.disallows(&https_url) {
                robots = https_robots;
                https_checked = true;
                if let Some(https_page) =
                    fetch_site_page(&https_url, "https_probe", search_id, &lead.name).await
                {
                    https_available = true;
                    if https_page.final_url.scheme() == "https" {
                        homepage = https_page.final_url;
                        home_html = https_page.html;
                        lead.website = Some(homepage.to_string());
                    }
                }
            }
        }
    }
    counts.fetched_ok += 1;
    lead.fetch_status = "ok".into();
    let html_lower = home_html.to_lowercase();
    if lead.email.is_none() {
        lead.email = extract_email_from_html(&home_html);
        if lead.email.is_none() {
            if let Some(contact_url) = find_contact_page_url(&homepage, &home_html) {
                if robots.disallows(&contact_url) {
                    info!(search_id, business=%lead.name, host=contact_url.host_str().unwrap_or(""), stage="contact_page", elapsed_ms=0, outcome="blocked_by_robots", "site request complete");
                } else if let Some(contact_page) =
                    fetch_site_page(&contact_url, "contact_page", search_id, &lead.name).await
                {
                    lead.email = extract_email_from_html(&contact_page.html);
                }
            } else {
                info!(search_id, business=%lead.name, host=homepage.host_str().unwrap_or(""), stage="contact_page", elapsed_ms=0, outcome="not_found", "site request complete");
            }
        }
    } else {
        info!(search_id, business=%lead.name, host=homepage.host_str().unwrap_or(""), stage="contact_page", elapsed_ms=0, outcome="skipped_email_already_present", "site request complete");
    }
    if lead.phone.is_none() {
        lead.phone = regex::Regex::new(r"(?i)(?:\+?\d[\d ()-]{7,}\d)")
            .unwrap()
            .find(&home_html)
            .map(|m| m.as_str().trim().to_string());
    }
    if lead.email.is_some() && !had_email {
        counts.emails_found += 1;
        lead.email_status = validate_email_logged(
            client,
            lead.email.as_deref().unwrap(),
            search_id,
            &lead.name,
        )
        .await;
        count_email_status(&mut counts, &lead.email_status);
    } else if lead.email.is_none() {
        info!(search_id, business=%lead.name, host=homepage.host_str().unwrap_or(""), stage="mx", elapsed_ms=0, outcome="skipped_no_email", "site request complete");
    }
    let mut reasons = Vec::new();
    if !https_available && https_checked {
        reasons.push(("No HTTPS".into(), cfg.signals.no_https));
    }
    if !html_lower.contains("name=\"viewport\"") && !html_lower.contains("name='viewport'") {
        reasons.push((
            "Not mobile-friendly (no viewport tag)".into(),
            cfg.signals.not_mobile_friendly,
        ));
    }
    if ![
        "google-analytics",
        "googletagmanager",
        "gtag(",
        " plausible.io",
        "matomo",
    ]
    .iter()
    .any(|needle| html_lower.contains(needle))
    {
        reasons.push(("No analytics found".into(), cfg.signals.no_analytics));
    }
    let year_re = regex::Regex::new(r"(?:©|&copy;|copyright)[^0-9]{0,16}(20[0-9]{2})").unwrap();
    if year_re
        .captures_iter(&html_lower)
        .filter_map(|c| c[1].parse::<i32>().ok())
        .any(|year| year < 2023)
    {
        reasons.push(("Outdated copyright year".into(), cfg.signals.old_copyright));
    }
    if !["book online", "online booking", "appointment"]
        .iter()
        .any(|needle| html_lower.contains(needle))
    {
        reasons.push((
            "No online booking found".into(),
            cfg.signals.no_online_booking,
        ));
    }
    if !html_lower.contains("<form") {
        reasons.push(("No contact form found".into(), cfg.signals.no_contact_form));
    }
    if !["intercom", "crisp.chat", "tawk.to", "livechat"]
        .iter()
        .any(|needle| html_lower.contains(needle))
    {
        reasons.push(("No chat widget found".into(), cfg.signals.no_chat_widget));
    }
    if lead
        .reasons
        .iter()
        .any(|r| r["label"] == "Brand-authorised or chain location")
    {
        reasons.push((
            "Brand-authorised or chain location".into(),
            cfg.signals.brand_affiliated,
        ));
    }
    (lead.score, lead.reasons) = scoring::score(reasons);
    info!(search_id, business=%lead.name, fetch_status=%lead.fetch_status, score=lead.score, "lead enrichment complete");
    (lead, counts)
}

#[derive(Default)]
struct RobotsPolicy {
    rules: Vec<(bool, String)>,
    deny_all: bool,
}

impl RobotsPolicy {
    fn disallows(&self, url: &reqwest::Url) -> bool {
        self.deny_all
            || self
                .rules
                .iter()
                .filter(|(_, path)| url.path().starts_with(path))
                .max_by_key(|(_, path)| path.len())
                .is_some_and(|(allowed, _)| !allowed)
    }
}

async fn load_robots_policy(site: &reqwest::Url, search_id: &str, business: &str) -> RobotsPolicy {
    let mut robots_url = site.clone();
    robots_url.set_path("/robots.txt");
    robots_url.set_query(None);
    robots_url.set_fragment(None);
    HOST_PACER.wait(&robots_url).await;
    let started = Instant::now();
    let response = match safe_site_get(robots_url).await {
        Ok(response) => response,
        Err(error) => {
            info!(search_id, business, host=site.host_str().unwrap_or(""), stage="robots_txt", elapsed_ms=started.elapsed().as_millis() as u64, outcome="unreachable", error=%error, "site request complete");
            return RobotsPolicy {
                deny_all: true,
                ..RobotsPolicy::default()
            };
        }
    };
    let (status, body, _) = response;
    info!(
        search_id,
        business,
        host = site.host_str().unwrap_or(""),
        stage = "robots_txt",
        elapsed_ms = started.elapsed().as_millis() as u64,
        outcome = if status.is_success() {
            "ok"
        } else {
            "http_error"
        },
        http_status = status.as_u16(),
        "site request complete"
    );
    if !status.is_success() {
        if status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error()
        {
            return RobotsPolicy {
                deny_all: true,
                ..RobotsPolicy::default()
            };
        }
        return RobotsPolicy::default();
    }
    let mut applies = false;
    let mut rules = Vec::new();
    for line in body.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.eq_ignore_ascii_case("user-agent") {
            applies = value.trim() == "*" || value.to_lowercase().contains("upsidebot");
        }
        if applies
            && (key.eq_ignore_ascii_case("disallow") || key.eq_ignore_ascii_case("allow"))
            && !value.is_empty()
        {
            rules.push((key.eq_ignore_ascii_case("allow"), value.to_string()));
        }
    }
    RobotsPolicy {
        rules,
        deny_all: false,
    }
}

async fn fetch_site_page(
    url: &reqwest::Url,
    stage: &'static str,
    search_id: &str,
    business: &str,
) -> Option<FetchedSitePage> {
    HOST_PACER.wait(url).await;
    let started = Instant::now();
    let result = safe_site_get(url.clone()).await;
    match result {
        Ok((status, body, final_url)) if status.is_success() => {
            info!(
                search_id,
                business,
                host = url.host_str().unwrap_or(""),
                stage,
                elapsed_ms = started.elapsed().as_millis() as u64,
                outcome = "ok",
                http_status = status.as_u16(),
                "site request complete"
            );
            Some(FetchedSitePage {
                html: body,
                final_url,
            })
        }
        Ok((status, _, _)) => {
            info!(
                search_id,
                business,
                host = url.host_str().unwrap_or(""),
                stage,
                elapsed_ms = started.elapsed().as_millis() as u64,
                outcome = "http_error",
                http_status = status.as_u16(),
                "site request complete"
            );
            None
        }
        Err(error) => {
            info!(search_id, business, host=url.host_str().unwrap_or(""), stage, elapsed_ms=started.elapsed().as_millis() as u64, outcome="request_error", error=%error, "site request complete");
            None
        }
    }
}

const MAX_SITE_RESPONSE_BYTES: usize = 1_048_576;
const MAX_SITE_REDIRECTS: usize = 5;

async fn safe_site_get(
    mut url: reqwest::Url,
) -> Result<(reqwest::StatusCode, String, reqwest::Url), String> {
    for redirect_count in 0..=MAX_SITE_REDIRECTS {
        let client = pinned_public_site_client(&url).await?;
        let response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|error| error.to_string())?;
        if response.status().is_redirection() {
            if redirect_count == MAX_SITE_REDIRECTS {
                return Err("too many website redirects".into());
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "website redirect has no valid Location".to_string())?;
            url = url.join(location).map_err(|error| error.to_string())?;
            continue;
        }
        let status = response.status();
        let final_url = response.url().clone();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_SITE_RESPONSE_BYTES as u64)
        {
            return Err("website response exceeds the 1 MiB limit".into());
        }
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
            if bytes.len().saturating_add(chunk.len()) > MAX_SITE_RESPONSE_BYTES {
                return Err("website response exceeds the 1 MiB limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&bytes).into_owned();
        return Ok((status, body, final_url));
    }
    Err("too many website redirects".into())
}

async fn pinned_public_site_client(url: &reqwest::Url) -> Result<reqwest::Client, String> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.port_or_known_default(), Some(80 | 443))
    {
        return Err("website URL must use HTTP(S) on port 80 or 443 without credentials".into());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "website URL has no host".to_string())?;
    let port = url.port_or_known_default().unwrap_or(0);
    let addresses: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Err(_) => tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| "website DNS lookup timed out".to_string())?
        .map_err(|error| error.to_string())?
        .collect(),
        Ok(address) => vec![SocketAddr::new(address, port)],
    };
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| !is_public_site_ip(address.ip()))
    {
        return Err("website resolves to a non-public IP address".into());
    }
    let contact = env::var("UPSIDE_CONTACT").unwrap_or_else(|_| "unknown".to_string());
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(format!("UpsideBot/0.1 (+mailto:{contact})"))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .resolve_to_addrs(host, &addresses);
    let client = builder.build().map_err(|error| error.to_string())?;
    Ok(client)
}

fn is_public_site_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || ip.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0)
                || (a == 198 && (b == 18 || b == 19 || b == 51 && c == 100))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_site_ip(IpAddr::V4(mapped));
            }
            let segments = ip.segments();
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
                || (segments[0] & 0xe000 != 0x2000)
                || (segments[0] == 0x2001 && segments[1] == 0x0db8))
        }
    }
}

struct FetchedSitePage {
    html: String,
    final_url: reqwest::Url,
}

fn find_contact_page_url(homepage: &reqwest::Url, html: &str) -> Option<reqwest::Url> {
    use std::sync::LazyLock;
    static LINKS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?is)<a\b[^>]*\bhref\s*=\s*(?:"([^"]+)"|'([^']+)'|([^\s>]+))[^>]*>"#)
            .unwrap()
    });
    for capture in LINKS.captures_iter(html) {
        let href = capture
            .get(1)
            .or_else(|| capture.get(2))
            .or_else(|| capture.get(3))?
            .as_str()
            .replace("&amp;", "&");
        let lower = href.to_lowercase();
        if !["contact", "kontakt", "impressum"]
            .iter()
            .any(|needle| lower.contains(needle))
        {
            continue;
        }
        let url = homepage.join(&href).ok()?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str() != homepage.host_str()
            || url.path() == homepage.path()
        {
            continue;
        }
        return Some(url);
    }
    None
}

async fn validate_email_logged(
    client: &reqwest::Client,
    email: &str,
    search_id: &str,
    business: &str,
) -> String {
    let started = Instant::now();
    let result = validate_email_mx(client, email).await;
    info!(search_id, business, stage="mx", elapsed_ms=started.elapsed().as_millis() as u64, outcome=%result, "site request complete");
    result
}

fn extract_email_from_html(html: &str) -> Option<String> {
    use std::sync::LazyLock;

    static NON_VISIBLE_BLOCKS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>|<style\b[^>]*>.*?</style\s*>")
            .unwrap()
    });
    static COMMENTS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?s)<!--.*?-->").unwrap());
    static BODY: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<body\b[^>]*>(.*?)</body\s*>").unwrap());
    static TAGS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?s)<[^>]*>").unwrap());
    static MAILTO: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?i)\bhref\s*=\s*['"]?mailto:([^'"?\s>]+)"#).unwrap()
    });
    static EMAIL: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)[a-z0-9.!#$%&'*+?^_`{|}~-]+@[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)*\.[a-z]{2,}",
        )
        .unwrap()
    });

    fn valid_candidate(candidate: &str) -> bool {
        if candidate.contains('/') || !EMAIL.is_match(candidate) {
            return false;
        }
        let tld = candidate
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        !matches!(
            tld.as_str(),
            "avif"
                | "bmp"
                | "css"
                | "cjs"
                | "csv"
                | "eot"
                | "gif"
                | "ico"
                | "jpeg"
                | "jpg"
                | "js"
                | "jsx"
                | "json"
                | "map"
                | "mjs"
                | "mp3"
                | "mp4"
                | "otf"
                | "pdf"
                | "png"
                | "svg"
                | "tar"
                | "tif"
                | "tiff"
                | "ttf"
                | "ts"
                | "tsx"
                | "webp"
                | "webmanifest"
                | "wasm"
                | "woff"
                | "woff2"
                | "xml"
                | "zip"
        )
    }

    let sanitized = NON_VISIBLE_BLOCKS.replace_all(html, " ");
    let sanitized = COMMENTS.replace_all(&sanitized, " ");
    for capture in MAILTO.captures_iter(&sanitized) {
        let candidate = capture[1]
            .trim()
            .trim_matches(|c: char| ".,;:)".contains(c));
        if valid_candidate(candidate) {
            return Some(candidate.to_string());
        }
    }

    let visible_region = BODY
        .captures(&sanitized)
        .and_then(|capture| capture.get(1).map(|body| body.as_str()))
        .unwrap_or(&sanitized);
    let visible_text = TAGS.replace_all(visible_region, " ");
    for found in EMAIL.find_iter(&visible_text) {
        let candidate = found.as_str();
        let before = visible_text[..found.start()].chars().next_back();
        let after = visible_text[found.end()..].chars().next();
        let address_char = |c: char| c.is_ascii_alphanumeric() || ".!#$%&'*+/?^_`{|}~-".contains(c);
        if before.is_some_and(address_char) || after.is_some_and(address_char) {
            continue;
        }
        if valid_candidate(candidate) {
            return Some(candidate.to_string());
        }
    }
    None
}

fn count_email_status(counts: &mut EnrichmentCounts, status: &str) {
    match status {
        "valid" => counts.emails_valid += 1,
        "invalid" => counts.emails_invalid += 1,
        _ => counts.emails_unverified += 1,
    }
}

async fn validate_email_mx(client: &reqwest::Client, email: &str) -> String {
    let Some(domain) = email.rsplit_once('@').map(|(_, domain)| domain) else {
        return "invalid".into();
    };
    let result = client
        .get("https://cloudflare-dns.com/dns-query")
        .query(&[("name", domain), ("type", "MX")])
        .header("accept", "application/dns-json")
        .send()
        .await;
    let Ok(response) = result else {
        return "unverified".into();
    };
    let Ok(value) = response.json::<serde_json::Value>().await else {
        return "unverified".into();
    };
    match value["Answer"].as_array() {
        Some(records) if records.iter().any(|r| r["type"].as_u64() == Some(15)) => "valid".into(),
        Some(_) => "invalid".into(),
        None if value["Status"].as_u64() == Some(3) => "invalid".into(),
        None => "unverified".into(),
    }
}

async fn cache_get<T: for<'de> Deserialize<'de>>(
    pool: Option<&PgPool>,
    key: &str,
    ttl_hours: i64,
) -> Option<T> {
    let ttl = Duration::from_secs(ttl_hours.max(0) as u64 * 3600);
    if let Some(value) = MEMORY_CACHE
        .lock()
        .await
        .get(key)
        .filter(|(stored, _)| stored.elapsed() <= ttl)
        .map(|(_, value)| value.clone())
    {
        return serde_json::from_value(value).ok();
    }
    let pool = pool?;
    let row = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT value FROM cache WHERE key=$1 AND fetched_at > now() - ($2 * interval '1 hour')",
    )
    .bind(key)
    .bind(ttl_hours)
    .fetch_optional(pool)
    .await
    .ok()??;
    MEMORY_CACHE
        .lock()
        .await
        .insert(key.to_owned(), (Instant::now(), row.clone()));
    serde_json::from_value(row).ok()
}

async fn cache_put<T: Serialize>(pool: Option<&PgPool>, key: &str, value: &T) {
    let Ok(json) = serde_json::to_value(value) else {
        return;
    };
    MEMORY_CACHE
        .lock()
        .await
        .insert(key.to_owned(), (Instant::now(), json.clone()));
    let Some(pool) = pool else {
        return;
    };
    if let Err(e) = sqlx::query("INSERT INTO cache(key,value,fetched_at) VALUES($1,$2,now()) ON CONFLICT(key) DO UPDATE SET value=excluded.value,fetched_at=excluded.fetched_at")
        .bind(key).bind(json).execute(pool).await { warn!("cache write failed: {e}"); }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[test]
    fn cors_allowlist_accepts_only_configured_exact_origins() {
        let origins = parse_cors_origins("https://example.vercel.app/,http://localhost:5173,*");
        assert!(origins.contains("https://example.vercel.app"));
        assert!(origins.contains("http://localhost:5173"));
        assert!(!origins.contains("*"));
        assert!(!origins.contains("https://other.vercel.app"));
    }

    #[test]
    fn search_rate_allows_two_per_minute_and_six_per_hour() {
        let now = Instant::now();
        let mut history = VecDeque::from([
            now - Duration::from_secs(5),
            now - Duration::from_secs(10),
            now - Duration::from_secs(70),
            now - Duration::from_secs(80),
            now - Duration::from_secs(90),
            now - Duration::from_secs(100),
        ]);
        assert!(!search_rate_allowed(&history, now));
        history.truncate(2);
        assert!(!search_rate_allowed(&history, now));
        history.pop_front();
        assert!(search_rate_allowed(&history, now));
    }

    fn place(
        addresstype: Option<&str>,
        place: Option<&str>,
        place_type: Option<&str>,
        place_rank: i32,
    ) -> NominatimPlace {
        NominatimPlace {
            lat: "1".into(),
            lon: "2".into(),
            address: None,
            display_name: None,
            addresstype: addresstype.map(str::to_owned),
            place: place.map(str::to_owned),
            place_type: place_type.map(str::to_owned),
            place_rank: Some(place_rank),
            boundingbox: None,
        }
    }

    #[test]
    fn accepts_cities_and_rejects_regions_and_countries() {
        let accepted = [
            place(Some("city"), None, None, 16), // Berlin, Germany
            place(Some("city"), None, None, 16), // Hamburg
            place(None, Some("city"), Some("administrative"), 8), // Singapore city-state
            place(Some("city"), None, None, 16), // San Francisco, California
            place(Some("town"), None, None, 16), // Jaipur
        ];
        assert!(accepted.iter().all(is_city_or_town));
        let rejected = [
            place(Some("state"), None, None, 8),   // California
            place(Some("country"), None, None, 4), // Germany
            place(Some("state"), None, None, 8),   // Rajasthan
        ];
        assert!(rejected.iter().all(|place| !is_city_or_town(place)));
    }

    #[test]
    fn photon_suggestions_keep_only_city_levels_and_format_region_country() {
        let response = PhotonResponse {
            features: vec![
                PhotonFeature {
                    properties: Some(PhotonProperties {
                        name: Some("Berlin".into()),
                        city: None,
                        state: Some("Berlin".into()),
                        country: Some("Germany".into()),
                        place_type: Some("city".into()),
                        osm_value: Some("administrative".into()),
                    }),
                },
                PhotonFeature {
                    properties: Some(PhotonProperties {
                        name: Some("Berlin Mitte".into()),
                        city: None,
                        state: Some("Berlin".into()),
                        country: Some("Germany".into()),
                        place_type: Some("district".into()),
                        osm_value: Some("suburb".into()),
                    }),
                },
                PhotonFeature {
                    properties: Some(PhotonProperties {
                        name: Some("Singapore".into()),
                        city: None,
                        state: None,
                        country: Some("Singapore".into()),
                        place_type: Some("city".into()),
                        osm_value: Some("city".into()),
                    }),
                },
            ],
        };
        let suggestions = photon_suggestions(response);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].label, "Berlin, Germany");
        assert_eq!(suggestions[1].label, "Singapore");
    }

    #[test]
    fn query_uses_named_objects_in_bounded_spatial_index_search() {
        let query = overpass_query("amenity=restaurant", 6000, 52.520008, 13.404954);
        assert!(
            query.starts_with("[out:json][timeout:11];nwr[\"amenity\"=\"restaurant\"][\"name\"](")
        );
        assert!(query.ends_with("out center tags qt 60;"));
        assert!(!query.contains("around:"));
    }

    #[test]
    fn cache_key_rounds_center_and_includes_category_radius() {
        assert_eq!(
            overpass_cache_key("Restaurants", 52.521, 13.405, 6000),
            "overpass:Restaurants:52.52:13.41:6000"
        );
    }

    #[test]
    fn snapshots_are_limited_to_the_two_demo_searches() {
        assert_eq!(
            demo_snapshot_name("Jaipur", "Auto repair"),
            Some("auto_repair_jaipur.json")
        );
        assert_eq!(
            demo_snapshot_name("Berlin, Germany", "Restaurants"),
            Some("restaurants_berlin.json")
        );
        assert_eq!(demo_snapshot_name("Paris", "Restaurants"), None);
    }

    #[test]
    fn extracts_only_visible_body_emails_or_mailto_links() {
        let html = r#"<html><head><script type="application/ld+json">{"email":"schema@example.com"}</script></head>
            <body><script>const hidden = 'script@example.com'; const pkg = 'formatjs/intl-segmenter@11.7.10';</script>
            <style>.x { content: 'style@example.com'; }</style><p>Contact visible@example.org</p></body></html>"#;
        assert_eq!(
            extract_email_from_html(html).as_deref(),
            Some("visible@example.org")
        );

        let mailto = r#"<body><a href="mailto:team@example.co?subject=Hello">Email us</a></body>"#;
        assert_eq!(
            extract_email_from_html(mailto).as_deref(),
            Some("team@example.co")
        );
    }

    #[test]
    fn rejects_npm_versions_slashes_assets_and_bad_tlds() {
        for false_positive in [
            "formatjs/intl-segmenter@11.7.10",
            "user@domain.com/file.png",
            "user@domain.com.png",
            "user@domain.com.js",
            "user@domain.c",
            "user@domain.123",
        ] {
            assert_eq!(
                extract_email_from_html(&format!("<body>{false_positive}</body>")),
                None,
                "{false_positive}"
            );
        }
    }

    #[test]
    fn extracts_a_normal_visible_email() {
        assert_eq!(
            extract_email_from_html("<body>Contact: <b>hello+sales@example-domain.com</b></body>")
                .as_deref(),
            Some("hello+sales@example-domain.com")
        );
    }
}
