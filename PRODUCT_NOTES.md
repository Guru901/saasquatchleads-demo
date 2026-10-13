# SaaSquatch Leads: focused product improvement

## What the reference product does

The public SaaSquatch Leads site positions the product as a lead generation workflow: search companies and people, enrich selected profiles, estimate company attributes, score prospects, save/export records, then support outreach. The local app in this repository is a narrower business finder named Upside. It searches OpenStreetMap via Nominatim and Overpass, checks publicly listed business websites for contact details and digital opportunity signals, ranks leads, and supports shortlisting and CSV export. It does not include SaaSquatch's account, credit, CRM, or person-search systems.

### Business model assessment

The public site presents a subscription ladder with monthly lead allowances, paid access to scraping and contact details, and higher tiers for advanced filters and email generation. It also promotes enrichment credits and a separate paid, human-led calling package. This aligns revenue with prospecting volume and adds expansion paths from self-service data to outreach assistance. The advertised benefits are breadth of profile data, cross-source checking, and paying to enrich only selected leads. The product opportunity is to improve conversion from data to action while keeping data provenance and uncertainty visible. In this prototype, the composer is a lightweight activation feature; it does not require or consume credits, and it is not represented as part of the public SaaSquatch product.

Strengths apparent from the public product positioning include a clear end-to-end workflow, a free entry tier, selective enrichment, multiple user segments, and an optional assisted service. Areas this local prototype cannot assess from the marketing page include actual match accuracy, source freshness, deliverability, retention, unit economics, and conversion to paid plans. The local tool also lacks the public product's broad contact and company intelligence, team workflow, and CRM integrations.

## Feature built

The lead detail panel now includes an editable outreach draft and copy action. The draft uses the highest positive score reason as a personalization cue. Where a reason points to a missing website listing, it explicitly says the OSM listing may be out of date. Every draft includes a review reminder, a low-pressure ask, and an opt-out style line. Drafts remain in component memory while the current results are open; no contact data or drafts are sent to a service or persisted remotely.

This closes the gap between finding a lead and taking action. It supports the product's existing selective-enrichment and export model while saving the operator from switching to a blank email for each prospect. Deterministic templates avoid unsupported AI claims and do not invent named decision makers.

## UX choices

- The composer lives in the existing lead detail panel, next to the evidence behind the score.
- The draft is editable before copying; subject and body are in one field to make it easy to move into the user's email client.
- Signals are framed as things to verify. Missing OSM data is not treated as evidence that a business lacks a website.
- Copy feedback is brief and local. If clipboard access is denied, the draft remains selectable and the UI explains the issue.
- The action is optional. Finding, filtering, shortlisting, and CSV export still work without using outreach.

## Current implementation architecture

These are technologies evidenced by the repository, not claims about the hosted SaaSquatch product:

- **Frontend:** React 19, TypeScript 6, Vite 8, CSS, lucide-react. It is a static client application. `VITE_API_URL` supplies the API base at build time; production deployments use the Vercel project's configured environment variable.
- **Backend:** Rust, Tokio, Ripress 2.5.1, SQLx, Reqwest, Serde, and tracing. Discovery/enrichment runs in Tokio background tasks.
- **Data sources:** OpenStreetMap Nominatim for geocoding; Overpass API for business discovery; the businesses' public websites for enrichment. The documented collection policy excludes sites whose terms prohibit scraping and requires respecting robots.txt and rate limits.
- **Database/cache:** PostgreSQL is optional. When `DATABASE_URL` is configured, the backend creates a JSONB cache table and caches geocoding and Overpass responses for 24 hours. Without Postgres, searches still run, but those cache entries are not persisted.
- **Transient state:** Search progress and results are held in an in-process `HashMap` behind a Tokio `RwLock`; a restart clears them. Shortlists are browser `localStorage` values. Outreach drafts currently live only in React state.
- **Caching/performance:** 24-hour source-response cache; 6 km search with a 12 km retry when fewer than eight results are found; bounded HTTP timeouts and mirror fallback; result cap of 40; client polls progress every 1.5 seconds. There is no shared distributed cache or queue in this repository.
- **Hosting/deployment:** No Docker, CI, hosting, or cloud-provider configuration is present in the workspace. Local run instructions are in `backend/README.md` and `frontend/README.md`. Therefore the production hosting provider and deployment process for the public domain cannot be verified from this source tree.

## Production architecture recommendation

For a small production rollout, serve the Vite build as static assets from **AWS S3 + CloudFront** and run the Rust API as a container on **AWS App Runner**. Use **Amazon RDS for PostgreSQL** for the existing cache and, before multi-instance use, durable search records. Keep credentials in AWS Secrets Manager. Deploy the frontend by building `frontend` and syncing `dist/` to a versioned S3 bucket, then invalidate CloudFront; deploy the backend from a pinned container image through App Runner. Add a database migration for durable jobs and use a managed queue such as SQS before scaling workers horizontally. Add tenant authentication, quotas, suppression/unsubscribe handling, and audit logs before enabling outbound sending. The feature implemented here only prepares copyable drafts and does not send messages.

This AWS setup is a recommendation, not a claim about the current site's infrastructure. It keeps static assets on CDN, isolates API scaling from the frontend, and gives the existing Postgres cache a managed home. For an initial demo, the current in-memory search state and optional cache are adequate; they are not suitable for restart-safe multi-instance production.

## Two-minute demo video

The repository has no screen recording or voiceover tooling. A 120-second silent, captioned product walkthrough is included at `artifacts/outreach-composer-demo.mp4`; its six cards show the flow and rationale. The narration below is ready for a human voiceover or live screen recording.

| Time | Show | Narration |
|---|---|---|
| 0:00–0:18 | SaaSquatch Leads public landing page, then the local lead finder | “SaaSquatch Leads brings discovery, enrichment, scoring, and activation into one workflow. This workspace is a smaller lead finder: it discovers local businesses from public OpenStreetMap data and scores possible digital upgrade opportunities.” |
| 0:18–0:38 | Search form and an example location/category search | “The biggest opportunity in this prototype was the handoff after discovery. Operators could inspect signals and export a shortlist, but still had to start each outreach message from scratch.” |
| 0:38–1:03 | Open a result, point to score reasons, then the outreach composer | “I added an outreach composer in the lead detail view. It uses the strongest positive signal already shown with the score, so the operator can see why the draft mentions that opportunity.” |
| 1:03–1:25 | Edit the draft, demonstrate copy feedback | “The draft is editable and copies in one action. It stays local in the browser session; there is no automatic sending, remote AI call, or extra enrichment credit.” |
| 1:25–1:43 | Show the OSM missing-website phrasing and review reminder | “Data gaps are uncertain, so the copy says a public listing may be outdated and asks the operator to verify the idea. That reduces the risk of turning incomplete source data into a false claim.” |
| 1:43–2:00 | Close on CSV export and the stack notes | “This is a focused five-hour feature: move from ranked lead to reviewed outreach faster. The implementation uses the existing React and TypeScript frontend. The current backend is Rust with optional PostgreSQL caching; the notes distinguish that from a proposed AWS production deployment.” |

## Verification

The frontend production build is the relevant check for this UI addition. No live scraping, contact, email-sending, or production-hosting behavior was added.
