/* Wrapper for the official exposed client. Workloads and score code stay unchanged. */
(() => {
    "use strict";
    const query = new URLSearchParams(location.search);
    const run = query.get("mb_run") || "manual";
    const browser = query.get("mb_browser") || "unspecified";
    const delay = Math.max(0, Number(query.get("mb_delay") || 5000));
    const barrier = query.get("mb_barrier") === "1";
    const originalUrl = location.href;
    const iterations = Number(query.get("iterationCount") || 10);
    const visibility = [];
    let startedAt = null;
    let startPerformance = null;
    let currentClient;
    // Speedometer validates its query parameters. Keep harness settings out of that parser.
    for (const key of Array.from(query.keys())) {
        if (key.startsWith("mb_")) query.delete(key);
    }
    history.replaceState(null, "", location.pathname + (query.size ? "?" + query.toString() : "") + location.hash);

    const metadata = () => ({
        run, browser, workload: "speedometer-3.1", tab: "1", iterations,
        user_agent: navigator.userAgent,
        location: originalUrl,
        epoch_ms: Date.now(), performance_ms: performance.now(),
        viewport: {width: innerWidth, height: innerHeight, device_pixel_ratio: devicePixelRatio},
        visibility: document.visibilityState, focused: document.hasFocus(),
    });
    async function post(event, extra = {}) {
        const response = await fetch("/api/event", {
            method: "POST", headers: {"Content-Type": "application/json"},
            body: JSON.stringify({...metadata(), event, ...extra}),
        });
        if (!response.ok) throw new Error("Collector HTTP " + response.status);
        return response.json();
    }
    async function waitForStart() {
        const deadline = Date.now() + 600000;
        while (Date.now() < deadline) {
            const response = await fetch("/api/start-time?run=" + encodeURIComponent(run));
            if (!response.ok) throw new Error("Start barrier HTTP " + response.status);
            const value = (await response.json()).start_epoch_ms;
            if (typeof value === "number" && Number.isFinite(value)) return value;
            await new Promise(resolve => setTimeout(resolve, 1000));
        }
        throw new Error("No start command received within ten minutes");
    }
    for (const kind of ["visibilitychange", "focus", "blur"]) {
        (kind === "visibilitychange" ? document : window).addEventListener(kind, () => {
            if (startedAt !== null) visibility.push({event: kind, epoch_ms: Date.now(), state: document.visibilityState, focused: document.hasFocus()});
        });
    }
    function install(client) {
        const originalStart = client.start;
        const originalFinish = client.didFinishLastIteration;
        const originalError = client.handleError;
        let requested = false;
        client.start = function (...args) {
            if (requested || this._isRunning) return;
            requested = true;
            (async () => {
                await post("ready", {source_commit: "1386415be8fef2f6b6bbdbe1828872471c5d802a", barrier});
                let scheduledAt = Date.now() + delay;
                if (barrier) {
                    scheduledAt = await waitForStart();
                }
                await new Promise(resolve => setTimeout(resolve, Math.max(0, scheduledAt - Date.now())));
                await post("started", {scheduled_start_epoch_ms: scheduledAt});
                startedAt = Date.now();
                startPerformance = performance.now();
                originalStart.apply(this, args);
            })().catch(error => post("error", {message: String(error), stack: error.stack}).catch(console.error));
        };
        client.didFinishLastIteration = function (metrics) {
            const finishedAt = Date.now();
            const elapsed = performance.now() - startPerformance;
            const result = originalFinish.call(this, metrics);
            // The official completion callback has finished all measured work and UI export.
            const completed = {
                started_epoch_ms: startedAt, finished_epoch_ms: finishedAt,
                elapsed_ms: elapsed, visibility_events: visibility,
                score: metrics.Score?.mean ?? null,
                score_delta: metrics.Score?.delta ?? null,
                score_values: metrics.Score?.values ?? [],
                displayed_score: document.getElementById("result-number")?.textContent,
                valid: document.getElementById("summary")?.classList.contains("valid"),
                metrics,
            };
            globalThis.mapBenchmarkResult = completed;
            post("completed", completed).catch(console.error);
            return result;
        };
        client.handleError = function (error) {
            post("error", {message: String(error), stack: error?.stack, visibility_events: visibility}).catch(console.error);
            return originalError.call(this, error);
        };
    }
    Object.defineProperty(globalThis, "benchmarkClient", {
        configurable: true,
        get() { return currentClient; },
        set(client) {
            currentClient = client;
            install(client);
            Object.defineProperty(globalThis, "benchmarkClient", {configurable: true, writable: true, value: client});
        },
    });
})();
