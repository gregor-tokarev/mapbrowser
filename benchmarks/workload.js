/* Controlled fixed-duration fixtures. Browser background throttling stays enabled. */
(async () => {
    "use strict";
    const params = new URLSearchParams(location.search);
    const run = params.get("run") || "manual";
    const browser = params.get("browser") || "unspecified";
    const tab = params.get("tab") || "1";
    const mode = params.get("mode") || "idle";
    const duration = Math.min(3600, Math.max(1, Number(params.get("duration") || 60)));
    const barrier = params.get("barrier") !== "0";
    const title = "Workload " + tab + " · " + mode;
    document.title = title;
    document.getElementById("heading").textContent = title;
    const status = document.getElementById("status");
    const rows = document.getElementById("rows");
    const canvas = document.getElementById("chart");
    const context = canvas.getContext("2d", {alpha: false});
    const scale = devicePixelRatio || 1;
    canvas.width = 620 * scale;
    canvas.height = 220 * scale;
    context.scale(scale, scale);
    let ticks = 0;
    let frames = 0;
    let checksum = 0;
    let timer;
    let frameRequest;
    let running = false;
    const frameIntervals = [];
    const visibility = [];
    let previousFrame;
    let startPerformance;
    let startedAt;
    let hiddenAtStart;
    let focusedAtStart;
    for (let index = 0; index < 160; index++) {
        const row = document.createElement("tr");
        for (const text of ["Document " + (index + 1), ["Work", "Research", "Planning", "Notes"][index % 4], String((index * 7919) % 10000)]) {
            const cell = document.createElement("td");
            cell.textContent = text;
            row.appendChild(cell);
        }
        rows.appendChild(row);
    }
    function draw(timestamp) {
        context.fillStyle = "#f6f8f4";
        context.fillRect(0, 0, 620, 220);
        const phase = timestamp / 1000;
        for (let index = 0; index < 96; index++) {
            const x = 20 + (index % 24) * 25;
            const y = 26 + Math.floor(index / 24) * 48 + Math.sin(phase + index * 0.2) * 14;
            context.fillStyle = ["#406c56", "#8ab496", "#b9c5a0", "#617e77"][index % 4];
            context.beginPath();
            context.arc(x, y, 6 + (index % 3), 0, Math.PI * 2);
            context.fill();
        }
    }
    draw(0);
    const metadata = () => ({
        run, browser, workload: "synthetic-" + mode, tab, mode, requested_duration_seconds: duration,
        epoch_ms: Date.now(), performance_ms: performance.now(),
        user_agent: navigator.userAgent,
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
            if (running) visibility.push({event: kind, epoch_ms: Date.now(), state: document.visibilityState, focused: document.hasFocus()});
        });
    }
    function tick() {
        ticks++;
        let value = ticks;
        for (let index = 0; index < 20000; index++) value = Math.imul(value ^ index, 1664525) + 1013904223 | 0;
        checksum = (checksum + value) | 0;
        const row = rows.children[(ticks * 17) % rows.children.length];
        row.lastChild.textContent = String(value >>> 0);
        row.classList.toggle("changed");
        if (ticks % 10 === 0) document.title = title + " · " + ticks;
    }
    function animate(timestamp) {
        if (!running) return;
        if (previousFrame !== undefined) frameIntervals.push(timestamp - previousFrame);
        previousFrame = timestamp;
        frames++;
        draw(timestamp - startPerformance);
        frameRequest = requestAnimationFrame(animate);
    }
    async function finish() {
        if (!running) return;
        running = false;
        clearInterval(timer);
        cancelAnimationFrame(frameRequest);
        const elapsed = performance.now() - startPerformance;
        status.textContent = "Completed";
        document.title = title + " · Completed";
        frameIntervals.sort((a, b) => a - b);
        const result = {
            started_epoch_ms: startedAt, finished_epoch_ms: Date.now(), elapsed_ms: elapsed,
            start_visibility: hiddenAtStart, start_focused: focusedAtStart,
            ticks, expected_ticks_without_throttling: Math.floor(duration), checksum,
            animation_frames: frames, frames_per_second: frames / (elapsed / 1000),
            frame_interval_median_ms: frameIntervals[Math.floor(frameIntervals.length / 2)] ?? null,
            frame_interval_p95_ms: frameIntervals[Math.floor(frameIntervals.length * .95)] ?? null,
            visibility_events: visibility,
        };
        globalThis.mapBenchmarkResult = result;
        await post("completed", result);
    }
    try {
        if (!["idle", "mixed", "canvas"].includes(mode)) throw new Error("mode must be idle, mixed or canvas");
        await post("ready", {barrier});
        status.textContent = "Ready. Waiting for the shared start time.";
        let scheduledAt = Date.now() + Number(params.get("delay_ms") || 5000);
        if (barrier) {
            scheduledAt = await waitForStart();
        }
        await new Promise(resolve => setTimeout(resolve, Math.max(0, scheduledAt - Date.now())));
        await post("started", {scheduled_start_epoch_ms: scheduledAt});
        startedAt = Date.now();
        startPerformance = performance.now();
        hiddenAtStart = document.visibilityState;
        focusedAtStart = document.hasFocus();
        running = true;
        status.textContent = "Running " + duration + " seconds";
        if (mode === "mixed") timer = setInterval(tick, 1000);
        if (mode === "canvas") frameRequest = requestAnimationFrame(animate);
        setTimeout(() => finish().catch(console.error), duration * 1000);
    } catch (error) {
        status.textContent = String(error);
        await post("error", {message: String(error), stack: error.stack});
    }
})();
