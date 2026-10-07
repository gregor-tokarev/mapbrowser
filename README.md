# Map Browser

Map Browser is a Mac web browser that looks and works like Arc, built to go easy on your battery.

Your tabs live in a sidebar instead of across the top. Group them into spaces, one for work and one for home, each with its own colour and its own logins. The page sits on a card inside the space's colour, and everything else stays out of the way.

## Why Map Browser

- **Arc's layout and feel.** Vertical tabs, favorites, pinned tabs, a command bar instead of a new-tab page, swipeable spaces, and light and dark themes that follow your Mac.
- **Gentle on battery.** The window doesn't redraw when nothing changes, tabs from your last session load only when you open them, and Chromium's background services that browse on their own are turned off.
- **Ads and trackers blocked.** Pages load fewer scripts, so ad-heavy sites use less battery, CPU and data.
- **Separate spaces, separate accounts.** Each space keeps its own cookies and site data, so the same site can be signed in to different accounts in different spaces.
- **Pages keep running.** Pages you've opened stay alive when you switch tabs or spaces, so chat and mail keep receiving messages. Map Browser never unloads them on its own.

## Using Map Browser

**Opening pages.** Press `⌘T` to open the command bar. Type an address or a search, pick an open tab from any space, or run an action such as Pin Tab, Clear Today Tabs or Switch to Space. A tab appears once you pick a result. `⌘L`, or a click on the address at the top of the sidebar, edits the current page's address.

**Today, pinned and favorites.**
- New tabs appear in **Today**, at the top of the list.
- `⌘D` pins a tab above the line. Right-click a tab to make it a **Favorite**, shown as a tile at the top.
- Drag tabs to reorder them or move them between sections.
- Closing a pinned tab or favorite (`⌘W` or its –) only puts it to sleep; it stays in the sidebar.
- `⌘W` closes a Today tab and `⇧⌘T` brings it back. The library button at the bottom left lists recently closed tabs.
- **Clear** (`⇧⌘K`) closes all Today tabs at once.

**Spaces.**
- Spaces appear as dots at the bottom of the sidebar.
- To switch, click a dot, swipe sideways with two fingers on the sidebar, or use `⌃1`–`⌃9` or `⌥⌘←` / `⌥⌘→`.
- The … beside a space's name renames it and sets its colour and icon. **Unload Space** puts all of its pages to sleep when you're done with them.
- The + at the bottom right creates a space. **Delete Space**, in the same … menu or on a right-click on a space's dot, removes a space with its tabs, logins and other site data, after asking. Your last space can't be deleted.

**Getting around.**

| Keys | Action |
|---|---|
| `⌘S` | Hide or show the sidebar |
| `⌘1`–`⌘9` | Go to a tab |
| `⌃Tab` | Back to the tab you were on. Hold `⌃` and press `Tab` again to go further back; the sidebar outlines the tab you'll get |
| `⌥⌘↑` / `⌥⌘↓` | Previous or next tab in the sidebar |
| `⌘R` | Reload |
| `⌘[` / `⌘]` | Back and forward |
| `⇧⌘C` | Copy the address |
| `⌘F` | Find on the page |
| `⌘+` / `⌘−` / `⌘0` | Zoom in, out, or back to actual size |

Web apps keep their own shortcuts: in an editor such as VS Code for the web or Google Docs, keys like `⇧⌘K`, `⌘[` or `⌘F` do what the app says. Opening, closing and switching tabs and spaces always works.

Drag the sidebar's edge to resize it; double-click the edge to reset it. The shield beside the address turns ad and tracker blocking off for that site.

Sign-in and payment windows, such as "Continue with Google" or PayPal, open as small windows and finish signing you in on the page that asked. Email and phone links open Mail and FaceTime.

Spaces, tabs and selections come back when you reopen Map Browser, and the window opens where you left it, at the same size. Your browsing data stays on your Mac.

## Status

Map Browser is an early prototype for macOS. It has no extensions, split view, sync, bookmarks, history, download manager or password manager yet, and it doesn't update itself. Battery life on par with Helium is the goal, but it hasn't been measured on a Mac yet.

## Credits

- Ad and tracker blocking uses the EasyList and EasyPrivacy filter lists by [The EasyList authors](https://easylist.to/), included under [CC BY-SA 3.0](licenses/EasyList-CC-BY-SA-3.0.txt), and Brave's [adblock-rust](licenses/adblock-rust-MPL-2.0.txt). The lists update with the app.
- Icons are from [Lucide](licenses/Lucide-ISC.txt).
- The text field is adapted from [GPUI](licenses/GPUI-Apache-2.0.txt).

Building from source: see [development notes](docs/development.md).
