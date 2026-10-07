// Exercise the callback wrapper without starting a browser or running a benchmark.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

(async () => {
    const events = [];
    let originalStarts = 0;
    let originalFinishes = 0;
    let replacedLocation;
    const context = {
        URLSearchParams, Date, performance, console, setTimeout,
        location: {search: "?startAutomatically&mb_run=unit-1&mb_browser=test&mb_delay=0", pathname: "/speedometer/", hash: "", href: "http://localhost/speedometer/?startAutomatically&mb_run=unit-1"},
        history: {replaceState(_a, _b, url) {replacedLocation = url;}},
        navigator: {userAgent: "Unit test"},
        innerWidth: 1000, innerHeight: 800, devicePixelRatio: 2,
        document: {
            visibilityState: "visible", hasFocus: () => true, addEventListener() {},
            getElementById(id) {return id === "summary" ? {classList: {contains: () => true}} : {textContent: "50"};},
        },
        addEventListener() {},
        async fetch(_url, options) {
            events.push(JSON.parse(options.body));
            return {ok: true, json: async () => ({ok: true})};
        },
    };
    context.window = context;
    vm.runInNewContext(fs.readFileSync(path.join(__dirname, "speedometer-harness.js"), "utf8"), context);
    context.benchmarkClient = {
        start() {originalStarts++;},
        didFinishLastIteration() {originalFinishes++;},
        handleError() {},
    };
    context.benchmarkClient.start();
    await new Promise(resolve => setTimeout(resolve, 20));
    assert.equal(originalStarts, 1);
    assert.deepEqual(events.map(item => item.event), ["ready", "started"]);
    assert.equal(events[0].run, "unit-1");
    assert.equal(events[0].iterations, 10);
    assert.equal(replacedLocation, "/speedometer/?startAutomatically=");
    context.benchmarkClient.didFinishLastIteration({Score: {mean: 50, delta: 1, values: [49, 51]}});
    assert.equal(originalFinishes, 1);
    assert.equal(events.at(-1).event, "completed");
    assert.equal(events.at(-1).score, 50);
    assert.equal(events.at(-1).metrics.Score.mean, 50);
    assert.equal(events.at(-1).valid, true);

    const barrierEvents = [];
    let polls = 0;
    let pollTimers = 0;
    let barrierStarts = 0;
    const barrierContext = {
        ...context,
        location: {...context.location, search: "?startAutomatically&mb_run=unit-2&mb_browser=test&mb_barrier=1"},
        setTimeout(callback, delay) {
            if (delay === 1000) pollTimers++;
            return setTimeout(callback, 0);
        },
        async fetch(url, options) {
            if (url.startsWith("/api/start-time?")) {
                polls++;
                return {ok: true, json: async () => ({start_epoch_ms: polls === 1 ? null : Date.now() + 50})};
            }
            barrierEvents.push(JSON.parse(options.body));
            return {ok: true, json: async () => ({ok: true})};
        },
    };
    barrierContext.window = barrierContext;
    vm.runInNewContext(fs.readFileSync(path.join(__dirname, "speedometer-harness.js"), "utf8"), barrierContext);
    barrierContext.benchmarkClient = {
        start() {barrierStarts++;}, didFinishLastIteration() {}, handleError() {},
    };
    barrierContext.benchmarkClient.start();
    await new Promise(resolve => setTimeout(resolve, 30));
    assert.equal(polls, 2);
    assert.equal(pollTimers, 1);
    assert.equal(barrierStarts, 1);
    assert.deepEqual(barrierEvents.map(item => item.event), ["ready", "started"]);
    await new Promise(resolve => setTimeout(resolve, 20));
    assert.equal(polls, 2, "Polling must stop after the future start time is known");
    console.log("Speedometer wrapper delegation, completion and pre-start polling checks passed.");
})().catch(error => {console.error(error); process.exitCode = 1;});
