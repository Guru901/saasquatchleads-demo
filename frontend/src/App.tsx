import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { Building2 } from "lucide-react";
import { Controller, useForm, useWatch } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Combobox } from "@/components/ui/combobox";
import { getCategories, getSearch, startSearch, suggestPlaces, MOCK } from "./api";
import { createSearchSchema, type SearchFormValues } from "./search-schema";
import type { Category, Lead, PlaceSuggestion, SearchState } from "./types";

const EMPTY_LEADS: Lead[] = [];
const SEARCH_CACHE_TTL = 24 * 60 * 60 * 1000;
const SEARCH_CACHE_KEY = "completed-searches-v1";
type CachedSearch = SearchState & { cachedAt: number };

function readSearchCache(key: string): CachedSearch | undefined {
    try {
        const saved = JSON.parse(localStorage.getItem(SEARCH_CACHE_KEY) ?? "{}") as Record<string, CachedSearch>;
        const entry = saved[key];
        return entry && Date.now() - entry.cachedAt < SEARCH_CACHE_TTL ? entry : undefined;
    } catch { return undefined; }
}

function writeSearchCache(key: string, value: CachedSearch) {
    try {
        const saved = JSON.parse(localStorage.getItem(SEARCH_CACHE_KEY) ?? "{}") as Record<string, CachedSearch>;
        const now = Date.now();
        const fresh = Object.fromEntries(Object.entries(saved)
            .filter(([, entry]) => now - entry.cachedAt < SEARCH_CACHE_TTL)
            .slice(-9));
        localStorage.setItem(SEARCH_CACHE_KEY, JSON.stringify({ ...fresh, [key]: value }));
    } catch { /* local storage may be unavailable or full */ }
}
const tier = (s: number) => (s >= 50 ? "hi" : s >= 25 ? "mid" : "lo");
const host = (u: string) => { try { return new URL(u).hostname; } catch { return u; } };
const DRAFT_SENDER_NAME = __DRAFT_SENDER_NAME__.trim() || "Your name";
const DRAFT_OFFER = __DRAFT_OFFER__.trim()
    || "I'm reaching out to see whether this is a priority for your team.";

const SIGNAL_PHRASES: Record<string, string> = {
    "Not mobile-friendly (no viewport tag)": "your website doesn't appear to be mobile-friendly",
    "No HTTPS": "your site isn't served over HTTPS",
    "No analytics found": "I couldn't confirm whether your site measures visitor activity",
    "Outdated copyright year": "the copyright information on your site may be out of date",
    "No online booking found": "online booking may not be available on your site",
    "No contact form found": "a contact form may not be available on your site",
    "No chat widget found": "live chat may not be available on your site",
};

function createOutreachDraft(lead: Lead) {
    const signal = lead.reasons.find((reason) => reason.points > 0)?.label;
    const signalPhrase = signal && SIGNAL_PHRASES[signal];
    const observation = signal?.toLowerCase().includes("openstreetmap")
        ? "I couldn't find a website in the public business listing I checked; that information may be out of date."
        : signalPhrase
            ? `I noticed that ${signalPhrase}. This may be worth checking.`
            : "I came across your business while researching local businesses in your category.";
    return `Subject: A quick question for ${lead.name}\n\nHi ${lead.name} team,\n\n${observation}\n\n${DRAFT_OFFER}\n\nWould you be open to a brief conversation? If this is already covered, please feel free to disregard.\n\nBest,\n${DRAFT_SENDER_NAME}`;
}

function createGmailComposeUrl(email: string, draft: string, name: string) {
    const lines = draft.split("\n");
    const subjectLine = lines[0] ?? "";
    const hasSubject = /^subject\s*:/i.test(subjectLine);
    const subject = hasSubject ? subjectLine.replace(/^subject\s*:\s*/i, "") : `A quick question for ${name}`;
    const body = hasSubject ? lines.slice(1).join("\n").replace(/^\n/, "") : draft;
    const params = new URLSearchParams({ view: "cm", fs: "1", to: email, su: subject, body });
    return `https://mail.google.com/mail/?${params.toString()}`;
}

function loadSaved(): Set<string> {
    try { return new Set(JSON.parse(localStorage.getItem("shortlist") ?? "[]")); } catch { return new Set(); }
}

function downloadCsv(rows: Lead[]) {
    const esc = (v: unknown) => `"${String(v ?? "").replace(/"/g, '""')}"`;
    const head = ["Name", "Category", "Address", "Website", "Phone", "Email", "Email status", "Score", "Reasons"];
    const body = rows.map((l) =>
        [l.name, l.category, l.address, l.website, l.phone, l.email, l.emailStatus, l.score,
        l.reasons.map((r) => `${r.label} (${r.points > 0 ? "+" : ""}${r.points})`).join("; ")].map(esc).join(","));
    const url = URL.createObjectURL(new Blob([[head.map(esc).join(","), ...body].join("\n")], { type: "text/csv" }));
    const a = document.createElement("a");
    a.href = url; a.download = "leads.csv"; a.click();
    URL.revokeObjectURL(url);
}

export default function App() {
    const queryClient = useQueryClient();
    const [debouncedLocation, setDebouncedLocation] = useState("");
    const [suggestionsOpen, setSuggestionsOpen] = useState(false);
    const [activeSuggestion, setActiveSuggestion] = useState(0);
    const [sid, setSid] = useState<string | null>(null);
    const [cachedSearch, setCachedSearch] = useState<SearchState | null>(null);
    const [activeCacheKey, setActiveCacheKey] = useState("");
    const [error, setError] = useState("");
    const [minScore, setMinScore] = useState(0);
    const [needEmail, setNeedEmail] = useState(false);
    const [needSite, setNeedSite] = useState(false);
    const [openId, setOpenId] = useState<string | null>(null);
    const [saved, setSaved] = useState<Set<string>>(loadSaved);
    const [drafts, setDrafts] = useState<Record<string, string>>({});
    const [searchStartedAt, setSearchStartedAt] = useState<number | null>(null);
    const [clockNow, setClockNow] = useState(() => Date.now());
    const [showProgress, setShowProgress] = useState(false);
    const progressHideTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
    const categoryQuery = useQuery({
        queryKey: ["categories"],
        queryFn: ({ signal }: { signal: AbortSignal }) => getCategories(signal),
        staleTime: SEARCH_CACHE_TTL,
    });
    const categories = categoryQuery.data ?? [];
    const categoriesLoading = categoryQuery.isPending;
    const categoriesError = categoryQuery.isError
        ? (categoryQuery.error as Error).message || "Could not load business categories."
        : categoryQuery.isSuccess && categories.length === 0
            ? "No business categories are available."
            : "";
    const searchSchema = useMemo(
        () => createSearchSchema(categories.map((item: Category) => item.label)),
        [categories],
    );
    const form = useForm<SearchFormValues>({
        resolver: zodResolver(searchSchema),
        defaultValues: { location: "", category: "" },
        mode: "onSubmit",
    });
    const location = useWatch({ control: form.control, name: "location" }) ?? "";
    const category = useWatch({ control: form.control, name: "category" }) ?? "";
    const placeQuery = useQuery({
        queryKey: ["place-suggestions", debouncedLocation.toLowerCase()],
        queryFn: ({ signal }: { signal: AbortSignal }) => suggestPlaces(debouncedLocation, signal),
        enabled: debouncedLocation.trim().length >= 3,
        staleTime: SEARCH_CACHE_TTL,
        retry: false,
    });
    const placeSuggestions = placeQuery.data ?? [];
    const suggestionsError = placeQuery.isError
        ? (placeQuery.error as Error).message || "Place suggestions are unavailable; you can still enter a city manually."
        : "";
    const searchQuery = useQuery({
        queryKey: ["search", sid],
        queryFn: ({ signal }: { signal: AbortSignal }) => getSearch(sid!, signal),
        enabled: sid !== null,
        staleTime: 0,
        retry: false,
        refetchInterval: (query: { state: { data?: SearchState; error: unknown } }) => {
            const status = query.state.data?.status;
            return query.state.error || status === "done" || status === "failed" ? false : 1500;
        },
        refetchIntervalInBackground: false,
    });
    const state: SearchState | null = sid
        ? (searchQuery.data?.id === sid ? searchQuery.data : null)
        : cachedSearch;
    const locationField = form.register("location");
    const searchIsRunning = searchStartedAt !== null
        && (!state || (state.status !== "done" && state.status !== "failed"));

    useEffect(() => {
        const query = location.trim();
        if (query.length < 3) {
            setDebouncedLocation("");
            return;
        }
        const timer = window.setTimeout(() => {
            setDebouncedLocation(query);
        }, 300);
        return () => clearTimeout(timer);
    }, [location]);

    useEffect(() => {
        if (state?.status === "done") {
            if (progressHideTimer.current) clearTimeout(progressHideTimer.current);
            progressHideTimer.current = setTimeout(() => setShowProgress(false), 600);
        } else if (state?.status === "failed") {
            setShowProgress(false);
        }
    }, [state?.status]);

    useEffect(() => {
        if (searchQuery.isError) {
            setError((searchQuery.error as Error).message || "Could not fetch results.");
            setSid(null);
            setSearchStartedAt(null);
            setShowProgress(false);
        }
    }, [searchQuery.isError, searchQuery.error]);

    useEffect(() => {
        if (!searchIsRunning) return;
        const interval = setInterval(() => setClockNow(Date.now()), 500);
        return () => clearInterval(interval);
    }, [searchIsRunning]);

    useEffect(() => () => {
        if (progressHideTimer.current) clearTimeout(progressHideTimer.current);
    }, []);

    useEffect(() => {
        try { localStorage.setItem("shortlist", JSON.stringify([...saved])); } catch { /* storage unavailable */ }
    }, [saved]);

    async function run(values: SearchFormValues) {
        setError(""); setOpenId(null);
        if (progressHideTimer.current) clearTimeout(progressHideTimer.current);
        const cacheKey = `${values.location.trim().toLocaleLowerCase()}|${values.category}`;
        const inMemory = queryClient.getQueryData<CachedSearch>(["completed-search", cacheKey]);
        const cached = inMemory ?? readSearchCache(cacheKey);
        if (cached && Date.now() - cached.cachedAt < SEARCH_CACHE_TTL) {
            queryClient.setQueryData(["completed-search", cacheKey], cached);
            setCachedSearch(cached);
            setSid(null);
            setSearchStartedAt(null);
            setShowProgress(false);
            return;
        }
        setCachedSearch(null);
        setActiveCacheKey(cacheKey);
        const startedAt = Date.now();
        setSearchStartedAt(startedAt);
        setClockNow(startedAt);
        setShowProgress(true);
        setSid(null);
        try { setSid(await startSearch(values.location.trim(), values.category)); }
        catch (err) {
            setError((err as Error).message);
            setSearchStartedAt(null);
            setShowProgress(false);
        }
    }

    useEffect(() => {
        const completed = searchQuery.data;
        if (!sid || !completed || completed.id !== sid || completed.status !== "done") return;
        const cacheKey = activeCacheKey;
        if (!cacheKey) return;
        const cached: CachedSearch = { ...completed, cachedAt: Date.now() };
        queryClient.setQueryData(["completed-search", cacheKey], cached);
        writeSearchCache(cacheKey, cached);
    }, [searchQuery.data, activeCacheKey, queryClient, sid]);

    const toggle = (id: string) => setSaved((prev) => {
        const next = new Set(prev);
        if (next.has(id)) next.delete(id); else next.add(id);
        return next;
    });

    const leads = state?.leads ?? EMPTY_LEADS;
    const shown = useMemo(() => leads
        .filter((l) => l.score >= minScore
            && (!needEmail || l.emailStatus === "valid" || l.emailStatus === "unverified")
            && (!needSite || !!l.website))
        .sort((a, b) => b.score - a.score), [leads, minScore, needEmail, needSite]);
    const open = leads.find((l) => l.id === openId);
    const exportRows = saved.size ? leads.filter((l) => saved.has(l.id)) : shown;
    const busy = searchIsRunning;
    const elapsedSeconds = searchStartedAt === null
        ? 0
        : Math.max(0, (clockNow - searchStartedAt) / 1000);
    const elapsedLabel = Math.floor(elapsedSeconds);
    const progressPercent = state?.status === "done"
        ? 100
        : state?.status === "enriching"
            ? (state.total > 0 ? 25 + 75 * state.done / state.total : 25)
            : Math.max(1, 25 * (1 - Math.exp(-elapsedSeconds / 8)));
  const stageMessage = searchStartedAt !== null && sid === null
    ? `Locating ${location}…`
    : state?.stage?.trim()
    || (state?.status === "done"
      ? "Search complete."
      : state?.status === "enriching"
        ? `Checking websites and scoring leads (${state.done}/${state.total})`
        : elapsedSeconds < 3
          ? `Locating ${location}…`
          : elapsedSeconds < 10
          && "");
    const stageKey = state?.stage?.toLowerCase() ?? "";
    const activeStep = state?.status === "done"
        ? 3
        : /website|enrich|scor/.test(stageKey) || state?.status === "enriching"
            ? 2
            : /area|geocod|locat/.test(stageKey)
                ? 0
                : /business|search|discover|map|overpass/.test(stageKey)
                    ? 1
                    : elapsedSeconds < 3 ? 0 : 1;
    const leadDraft = open ? (drafts[open.id] ?? createOutreachDraft(open)) : "";

    function choosePlace(label: string) {
        form.setValue("location", label, { shouldDirty: true, shouldTouch: true, shouldValidate: true });
        setSuggestionsOpen(false);
    }

    function handleLocationKeyDown(event: KeyboardEvent<HTMLInputElement>) {
        if (!suggestionsOpen || !placeSuggestions.length) {
            if (event.key === "Escape") setSuggestionsOpen(false);
            return;
        }
        if (event.key === "ArrowDown") {
            event.preventDefault();
            setActiveSuggestion((index) => (index + 1) % placeSuggestions.length);
        } else if (event.key === "ArrowUp") {
            event.preventDefault();
            setActiveSuggestion((index) => (index - 1 + placeSuggestions.length) % placeSuggestions.length);
        } else if (event.key === "Enter") {
            event.preventDefault();
            const selected = placeSuggestions[activeSuggestion];
            if (selected) choosePlace(selected.label);
        } else if (event.key === "Escape") {
            event.preventDefault();
            setSuggestionsOpen(false);
        }
    }

    return (
        <div className="app">
            <header className="topbar">
                <div className="brand"><span className="brandmark"><Building2 size={19} strokeWidth={2.2} aria-hidden="true" /></span><div><h1>Upside</h1><p>Business opportunity finder</p></div></div>
                {MOCK && <span className="demo">DEMO DATA</span>}
            </header>

            <section className="intro">
                <p className="eyebrow">Lead discovery</p>
                <h2>Find businesses ready to grow.</h2>
                <p>Discover local businesses and uncover the digital upgrades they need most.</p>
            </section>

            <form className="search" onSubmit={form.handleSubmit(run)}>
                <label className="field">Location
                    <div className="field-dropdown-anchor">
                        <input {...locationField} onChange={(event) => { locationField.onChange(event); setActiveSuggestion(0); setSuggestionsOpen(true); }} onKeyDown={handleLocationKeyDown} onFocus={() => { setActiveSuggestion(0); setSuggestionsOpen(true); }} onBlur={(event) => { locationField.onBlur(event); window.setTimeout(() => setSuggestionsOpen(false), 150); }} placeholder="City or town" autoComplete="off" aria-autocomplete="list" aria-expanded={suggestionsOpen && placeSuggestions.length > 0} aria-controls="place-suggestions" aria-activedescendant={suggestionsOpen && placeSuggestions[activeSuggestion] ? `place-suggestion-${activeSuggestion}` : undefined} aria-invalid={!!form.formState.errors.location} />
                        {suggestionsOpen && placeSuggestions.length > 0 && (
                            <ul className="place-suggestions" id="place-suggestions" role="listbox">
                                {placeSuggestions.map((suggestion: PlaceSuggestion, index: number) => <li id={`place-suggestion-${index}`} key={suggestion.label} className={index === activeSuggestion ? "highlighted" : ""} role="option" aria-selected={index === activeSuggestion} onPointerEnter={() => setActiveSuggestion(index)}><button type="button" onMouseDown={(event) => event.preventDefault()} onClick={() => choosePlace(suggestion.label)}>{suggestion.label}</button></li>)}
                            </ul>
                        )}
                    </div>
                    {form.formState.errors.location && <span className="field-error">{form.formState.errors.location.message}</span>}
                </label>
                <label className="field">Business category
                    <Controller control={form.control} name="category" render={({ field }) => (
                        <Combobox
                            items={categories.map((item: Category) => item.label)}
                            value={field.value}
                            onValueChange={field.onChange}
                            disabled={categoriesLoading || !!categoriesError || !categories.length}
                            name={field.name}
                            inputRef={field.ref}
                            onBlur={field.onBlur}
                            placeholder={categoriesLoading ? "Loading categories…" : "Select or search categories…"}
                            invalid={!!form.formState.errors.category}
                        />
                    )} />
                    {form.formState.errors.category && <span className="field-error">{form.formState.errors.category.message}</span>}
                </label>
                <button className="button" disabled={busy || categoriesLoading || !!categoriesError || !category}>{busy ? "Searching…" : "Find businesses"}</button>
            </form>

            {categoriesError && <p className="error" role="alert">{categoriesError}</p>}
            {suggestionsError && <p className="muted" role="status">{suggestionsError}</p>}
            {error && <p className="error">{error}</p>}
            {state?.status === "failed" && <p className="error">{state.error ?? "Search failed."}</p>}
            {state?.place && <p className="search-place">Showing results near {state.place}</p>}
            {showProgress && (busy || state?.status === "done") && (
                <section className="search-progress" aria-label="Search progress">
                    <p className="progress-stage" aria-live="polite">{stageMessage}</p>
                    <ol className="progress-steps" aria-label="Search steps">
                        {["Find area", "Find businesses", "Check websites & score"].map((step, index) => {
                            const completed = index < activeStep;
                            const active = index === activeStep && activeStep < 3;
                            return (
                                <li key={step} className={completed ? "complete" : active ? "active" : "pending"} aria-current={active ? "step" : undefined}>
                                    <span className="step-marker" aria-hidden="true">{completed || activeStep === 3 ? "✓" : index + 1}</span>
                                    <span>{step}</span>
                                </li>
                            );
                        })}
                    </ol>
                    <div className="progress" role="progressbar" aria-label="Search completion" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(progressPercent)}>
                        <div style={{ width: `${progressPercent}%` }} />
                    </div>
                    {busy && <p className="progress-elapsed">{elapsedLabel}s elapsed</p>}
                </section>
            )}

            {leads.length > 0 && (
                <section className="filters">
                    <label className="filter-score">Min score <b>{minScore}</b>
                        <input type="range" min={0} max={100} step={5} value={minScore} onChange={(e) => setMinScore(+e.target.value)} />
                    </label>
                    <label className="check"><input type="checkbox" checked={needEmail} onChange={(e) => setNeedEmail(e.target.checked)} /> Has email</label>
                    <label className="check"><input type="checkbox" checked={needSite} onChange={(e) => setNeedSite(e.target.checked)} /> Has website</label>
                    <span className="spacer" />
                    <span className="count">{shown.length} of {leads.length} businesses</span>
                    <button className="button secondary" onClick={() => downloadCsv(exportRows)} disabled={!exportRows.length}>
                        Export CSV ({exportRows.length}{saved.size ? " shortlisted" : ""})
                    </button>
                </section>
            )}

            <main className={open ? "split" : ""}>
                {leads.length === 0 ? (
                    <p className="empty">{busy ? "" : <><span className="empty-icon">⌕</span>Run a search to see businesses ranked by digital-upgrade upside.</>}</p>
                ) : (
                    <div className="tablewrap">
                        <table>
                            <thead><tr><th>Score</th><th>Business</th><th>Contact</th><th>Website</th><th>Top signal</th><th /></tr></thead>
                            <tbody>
                                {shown.map((l) => (
                                    <tr key={l.id} className={l.id === openId ? "active" : ""} onClick={() => setOpenId(l.id)}>
                                        <td><span className={`badge ${tier(l.score)}`}>{l.score}</span></td>
                                        <td><b>{l.name}</b><small>{l.address}</small></td>
                                        <td>
                                            {l.email ? <>{l.email} <i className={`dot ${l.emailStatus}`} title={l.emailStatus} /></> : <small>No email found</small>}
                                            <small>{l.phone ?? "No phone"}</small>
                                        </td>
                                        <td>
                                            {l.website
                                                ? <a href={l.website} target="_blank" rel="noreferrer" onClick={(e) => e.stopPropagation()}>{host(l.website)}</a>
                                                : <small>None</small>}
                                        </td>
                                        <td><small>{l.reasons[0]?.label ?? "No listed contact gaps"}</small></td>
                                        <td>
                                            <button className={`star ${saved.has(l.id) ? "saved" : ""}`} aria-label={saved.has(l.id) ? "Remove from shortlist" : "Add to shortlist"} title={saved.has(l.id) ? "Remove from shortlist" : "Add to shortlist"} onClick={(e) => { e.stopPropagation(); toggle(l.id); }}>
                                                {saved.has(l.id) ? "★" : "☆"}
                                            </button>
                                        </td>
                                    </tr>
                                ))}
                            </tbody>
                        </table>
                    </div>
                )}

                {open && (
                    <aside>
                        <button className="close" onClick={() => setOpenId(null)} aria-label="Close">×</button>
                        <div className="aside-head"><h2>{open.name}</h2>
                            <p className="subline">{open.category} · {open.address}</p></div>
                        <span className={`badge big ${tier(open.score)}`}>{open.score}</span>
                        <h3>Why this score</h3>
                        {open.reasons.length
                            ? <ul className="reasons">{open.reasons.map((r) => <li key={r.label}><span>{r.label}</span><b>{r.points > 0 ? "+" : ""}{r.points}</b></li>)}</ul>
                            : <p className="muted">No listed contact gaps found.</p>}
                        <h3>Contact</h3>
                        <p className="contact">{open.email ?? "No email"} <small className="muted">({open.emailStatus})</small></p>
                        <p className="contact">{open.phone ?? "No phone"}</p>
                        <div className="outreach-heading">
                            <h3>Outreach draft</h3>
                            <span>Review before sending</span>
                        </div>
                        <p className="draft-context">Personalized from the top public signal. Verify the opportunity before contacting them.</p>
                        <textarea
                            aria-label={`Outreach draft for ${open.name}`}
                            className="draft"
                            value={leadDraft}
                            onChange={(e) => setDrafts((prev) => ({ ...prev, [open.id]: e.target.value }))}
                        />
                        {open.email
                            ? <a className="button secondary send-email" href={createGmailComposeUrl(open.email, leadDraft, open.name)} target="_blank" rel="noopener noreferrer">Send email</a>
                            : <button className="button secondary send-email" type="button" disabled title="This lead has no email address">Send email</button>}
                    </aside>
                )}
            </main>
            <footer className="attribution"><a href="https://www.openstreetmap.org/copyright" target="_blank" rel="noreferrer">Data © OpenStreetMap contributors</a>{state?.source && state.source !== "live" ? " · backup source" : ""}</footer>
        </div>
    );
}
