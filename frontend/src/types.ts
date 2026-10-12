export type EmailStatus = "valid" | "unverified" | "invalid" | "none";
export type FetchStatus = "ok" | "unreachable" | "blocked" | "no_website";
export interface Reason { label: string; points: number }
export interface Lead {
    id: string; name: string; category: string; address: string;
    website?: string; phone?: string; email?: string;
    emailStatus: EmailStatus; fetchStatus: FetchStatus;
    score: number; reasons: Reason[];
}
export type SearchStatus = "discovering" | "enriching" | "done" | "failed";
export interface SearchState {
    id: string; status: SearchStatus; done: number; total: number;
    leads: Lead[]; error?: string; stage?: string;
    source?: "live" | "nominatim" | "cache" | "snapshot";
    place?: string;
}

export interface Category { id: string; label: string }
export interface PlaceSuggestion { label: string }
