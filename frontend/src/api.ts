import type { Category, Lead, PlaceSuggestion, SearchState } from "./types";

/*
 * API CONTRACT (implement this in the Ripress backend)
 *   POST /api/searches      body: { location, category }  ->  { id }
 *   GET  /api/searches/:id  ->  SearchState (types.ts)
 * The backend runs discovery + enrichment as a background job. The client polls
 * GET every ~1.5s until status is "done" or "failed". `leads` should contain
 * every lead enriched so far, so rows stream in while the job runs.
 * Remember CORS on the backend (app.use_cors).
 *
 * Env: VITE_USE_MOCK=false to use the real API, VITE_API_URL to set its base URL.
 */
const BASE = import.meta.env.VITE_API_URL?.replace(/\/$/, "");
if (!BASE) throw new Error("VITE_API_URL must be set in the frontend environment.");
export const MOCK = false;

const lead = (id: string, name: string, address: string, website: string | undefined,
    phone: string | undefined, email: string | undefined, emailStatus: Lead["emailStatus"],
    reasons: [string, number][], fetchStatus: Lead["fetchStatus"] = "ok"): Lead => ({
        id, name, category: "Dental clinic", address, website, phone, email, emailStatus, fetchStatus,
        reasons: reasons.map(([label, points]) => ({ label, points })),
        score: reasons.reduce((s, [, p]) => s + p, 0),
    });

const SAMPLE: Lead[] = [
    lead("1", "Brightline Dental", "C-Scheme, Jaipur", "http://brightline-dental.example", "+91 141 000 0001", "hello@brightline-dental.example", "valid",
        [["No HTTPS", 15], ["Not mobile-friendly", 15], ["Built on Wix", 5], ["No analytics found", 10], ["No online booking", 15], ["Footer copyright 2017", 10]]),
    lead("2", "Orchid Smile Clinic", "Malviya Nagar, Jaipur", "https://orchidsmile.example", "+91 141 000 0002", "info@orchidsmile.example", "unverified",
        [["No online booking", 15], ["No analytics found", 10], ["Footer copyright 2019", 10], ["No contact form", 10]]),
    lead("3", "Meridian Dental Care", "Vaishali Nagar, Jaipur", "https://meridiandental.example", "+91 141 000 0003", "contact@meridiandental.example", "valid",
        [["No analytics found", 10], ["No online booking", 15]]),
    lead("4", "DentaCare Plus", "Mansarovar, Jaipur", "http://dentacareplus.example", "+91 141 000 0004", undefined, "none",
        [["No HTTPS", 15], ["Not mobile-friendly", 15], ["Footer copyright 2016", 10], ["No analytics found", 10], ["No chat widget", 10]]),
    lead("5", "Lotus Family Dentistry", "Tonk Road, Jaipur", "https://lotusfamilydental.example", "+91 141 000 0005", "desk@lotusfamilydental.example", "valid",
        [["No chat widget", 10]]),
    lead("6", "Greenfield Dental", "Bani Park, Jaipur", "https://greenfielddental.example", undefined, undefined, "none", [], "unreachable"),
];

const started: Record<string, number> = {};
const startMock = async () => { const id = String(Date.now()); started[id] = Date.now(); return id; };
const pollMock = async (id: string): Promise<SearchState> => {
    const t = (Date.now() - started[id]) / 1000;
    const total = SAMPLE.length;
    const done = t < 1.5 ? 0 : Math.min(total, Math.floor((t - 1.5) / 1.2));
    const status = t < 1.5 ? "discovering" : done < total ? "enriching" : "done";
    return { id, status, done, total, leads: SAMPLE.slice(0, done) };
};

async function startReal(location: string, category: string): Promise<string> {
    const r = await fetch(`${BASE}/api/searches`, {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ location, category }),
        signal: AbortSignal.timeout(50_000),
    });
    if (!r.ok) {
        const body = await r.json().catch(() => ({})) as { error?: string };
        throw new Error(body.error || `Could not start search (${r.status})`);
    }
    return (await r.json()).id as string;
}
async function pollReal(id: string, signal?: AbortSignal): Promise<SearchState> {
    const timeout = AbortSignal.timeout(10_000);
    const r = await fetch(`${BASE}/api/searches/${id}`, { signal: signal ? AbortSignal.any([signal, timeout]) : timeout });
    if (!r.ok) throw new Error(`Could not fetch results (${r.status})`);
    return r.json();
}

export async function getCategories(signal?: AbortSignal): Promise<Category[]> {
    const timeout = AbortSignal.timeout(8_000);
    const r = await fetch(`${BASE}/api/categories`, { signal: signal ? AbortSignal.any([signal, timeout]) : timeout });
    if (!r.ok) throw new Error(`Could not load business categories (${r.status})`);
    return r.json() as Promise<Category[]>;
}

export async function suggestPlaces(query: string, signal?: AbortSignal): Promise<PlaceSuggestion[]> {
    const timeout = AbortSignal.timeout(8_000);
    const r = await fetch(`${BASE}/api/places/suggest?q=${encodeURIComponent(query)}`, { signal: signal ? AbortSignal.any([signal, timeout]) : timeout });
    if (!r.ok) {
        const body = await r.json().catch(() => ({})) as { error?: string };
        throw new Error(body.error || `Could not load place suggestions (${r.status})`);
    }
    return r.json() as Promise<PlaceSuggestion[]>;
}

export const startSearch = (location: string, category: string) => (MOCK ? startMock() : startReal(location, category));
export const getSearch = (id: string, signal?: AbortSignal) => (MOCK ? pollMock(id) : pollReal(id, signal));
