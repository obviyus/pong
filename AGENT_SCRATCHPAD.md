## Current Patterns
- None yet.
Date: 2026-02-12
Issue: Baseline perf cost from `sleep_with_shutdown` 25ms quantum across 33 workers (~1320 wakeups/s) + per-render string/Vec churn in ui.
Correction: Prefer evented shutdown wait + fixed render tick with preallocated row buffers.
Preference: Optimize hot path first; avoid adding abstractions before repeated use.
Action: Keep perf audits focused on worker loop and ui render allocations.
