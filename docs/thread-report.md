Thread completion report, September 12, 2026

We built a working macOS browser prototype with Rust, GPUI and embedded Chromium.
The thread ended with the memory evaluation stopped at the user's request. The
latest memory changes remain in the source and signed release bundle. Their
memory savings, Arc parity and battery benefits are not established.

The agreed product scope is a sidebar with vertical tabs and multiple spaces.
Each space has separate website cookies, logins and local storage through its
own Chromium profile. Open pages keep running when their space is inactive,
subject to Chromium's normal background scheduling. The first version is macOS
only, with no extensions or split view. The motivation is Arc-like organization
with energy use closer to Chrome or Helium. That remains an acceptance target.

| Work completed | Result and limits |
|---|---|
| Browser application | Rust and GPUI interface around CEF Chromium, with sandboxed helper processes and a native macOS event-loop bridge. |
| Tabs and spaces | Create, select, close and restore tabs; create and rename spaces; maintain separate profile directories and saved selections. Blank new tabs use GPUI without creating a page renderer. |
| Navigation and input | Address entry and search, keyboard shortcuts, back, forward, reload, native web-page editing and external HTTP/S links opening in new tabs. |
| Workspace animation | Implemented a 240 ms slide between existing views, with Reduce Motion support. Earlier native checks covered repeated switches, preserved scroll position and switching after resize. |
| Native integration fixes | Fixed input routing and shutdown behavior. Added a one-time native-view geometry adjustment to resolve the observed initial blank-page problem. Broader lifecycle coverage is still needed. |
| Build workflow | Added `make run`, `make build`, `make dev`, `make test`, `make check` and supporting targets. The app is bundled with CEF and locally signed. |
| Google sign-in crash investigation | Added the missing Bluetooth privacy description used by Chromium's cross-device passkey flow. This addressed the identified macOS privacy-termination cause; it did not enable Apple Passwords or prove a complete Google sign-in. |
| Apple Passwords preparation | Added optional signing-identity and provisioning-profile support, validation tests, HTTP/S URL handling, documentation and a native-window fallback patch for a custom CEF build. The normal app cannot yet use Apple Passwords passkeys. |
| Diagnostic tools | Added local storage and heartbeat fixtures, animation-log analysis, browser workload serving, process memory/CPU sampling, foreground validation and saved reports. |
| Memory changes | Vendored GPUI 0.2.2, made Metal path textures allocate on demand, limited the shell to two drawables, and made the Metal layer honor opaque windows. Built and signed the candidate. |

The prototype checks established that two spaces could hold different synthetic
cookie and local-storage values during a session. Server logs showed the same
hidden page instances continuing to send heartbeat requests. Local storage
survived a restart. Cookies did not survive the tested restart after Keychain
access was denied, so persistent website logins cannot be marked complete.

Six completed workspace transitions on an earlier build took approximately
241.5 to 247.4 ms. The median interval between GPUI render callbacks was 8.33 ms,
with a 9.17 ms p95. These are callback timings, not proof of displayed frame
cadence. The final memory candidate received path drawing, resizing and
workspace endpoint checks, but not a complete repeat of that animation study.
See [prototype test results](test-results.md) for the build-specific evidence.

The latest candidate passed 15 Rust tests, 15 bundle/signing tests and two
memory-analysis tests. Formatting and checks of all Cargo targets also passed.
The native renderer example verified the first path after an empty scene,
resizing with a path and hiding paths again. Passing those checks does not
establish memory savings or production readiness.

The four-browser diagnostic produced the following historical observations:

| Browser | Speedometer 3.1 median | One loaded tab, MiB | Ten loaded tabs, MiB |
|---|---:|---:|---:|
| Mapbrowser | 56.34 | 601.6 | 1012.9 |
| Arc | 44.97 | 448.0 | 1043.7 |
| Chrome | 45.35 | 378.1 | 1047.8 |
| Safari | 43.95 | 312.3 | 544.9 |

These are local diagnostic results, not a controlled general ranking.
Mapbrowser's accepted one-tab run followed previous Speedometer and ten-tab
attempts, so its 601.6 MiB figure was a warmed-process observation. Arc retained
its existing profile and an extension renderer, and used a different Chromium
version. Safari suspended or did not deliver timers in two background test
tabs. Other applications, including a busy VPN, added contention. These limits
matter when comparing the numbers. See the [corrected benchmark report](../artifacts/benchmarks/2026-09-12/REPORT.md).

The following conclusions replaced the earlier assumptions:

- The benchmarked Mapbrowser binary was already an optimized release build.
  Switching from debug to release was not an available explanation or fix for
  those memory figures. Rust build settings also do not recompile the prebuilt
  Chromium helpers.
- The high one-tab total included the main process, renderers and Metal
  compiler services after previous browsing. The main process contains both
  GPUI and CEF. The saved process breakdown identifies where memory was charged,
  but does not identify an allocation-level cause or prove a leak.
- We incorrectly treated the top-level Activity Monitor app row as a
  whole-browser Energy Impact reading. After the user's correction, we withdrew
  that comparison and excluded all 56 original readings. The separate process
  CPU and memory measurements already included attributed helpers.
- Kernel CPU-energy counters do not measure total device energy or battery
  life. Neither the Speedometer scores nor the CPU watts establish lower
  battery drain than Arc, Chrome, Safari or Helium.
- Unused texture capacity is not the same as committed physical memory. The
  first lazy-texture experiment did not justify claiming its theoretical
  allocation reduction as actual RAM savings.

During the later memory work, valid measurements of the original release build
showed 986.2 MiB with ten warmed test tabs and 513.9 MiB after closing nine.
These are baseline values, not results of the optimization. The final
candidate's after-one run loaded its page but failed foreground validation.
The evaluation then stopped at the user's request, and the task's test servers
were shut down. No valid final before/after pair establishes savings. The
[memory evaluation record](../artifacts/memory-optimization/REPORT.md) retains the
baselines, exclusions and validation status.

Two platform issues remain separate from performance work. The bundled CEF
uses the shared `Chromium Safe Storage` encryption-key item, which explains the
repeated macOS Keychain prompts. Separate workspace profiles do not change that
key name. An app-specific Keychain configuration needs a compatible CEF runtime
and bindings update. Denied access prevented reliable encrypted cookie
persistence in the recorded tests. See [Keychain findings](keychain.md).

For Google passkeys stored in Apple Passwords or iCloud Keychain, Touch ID is
the authorization method. The current ad hoc build does not have Apple's
approved browser passkey entitlement and provisioning configuration. The
prepared native-window patch also needs to be compiled into a custom CEF
runtime. No existing Google passkey authentication was verified. See
[passkey implementation status](passkeys.md).

The handoff is the source tree, locally signed release app, Makefile, tests,
fixtures, prepared CEF patch and saved reports. Quit an existing app before
rebuilding, then use `make run` to build and open the release app. The remaining
acceptance work is reliable login persistence, real Apple Passwords passkey
integration, broader native-browser QA, and matched memory and energy
measurements. Evaluation remains stopped; this report does not restart it.
