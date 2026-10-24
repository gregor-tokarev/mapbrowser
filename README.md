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

**Opening pages.** Press `⌘T` to open the command bar. Type an address or a search, pick an open tab from any space, a tab you just closed or a page you've visited before, or run an action such as Pin Tab, Mute Tab, Clear Today or Switch to Work. A tab appears once you pick a result. Type the name of an open tab or of a space, or the address of an open tab, and Return goes there instead of searching. `⌘L`, or a click on the address at the top of the sidebar, edits the current page's address; `⌘Return` opens the edited address in a new tab instead.

Home-network addresses such as `192.168.1.1`, `nas.local` or `router/` open as they are. Start with `?` to search for something that looks like an address. Typed `mailto:` and `tel:` addresses open Mail and FaceTime. `⌘Z` and `⇧⌘Z` undo and redo typing in the command bar and every other field of the browser, so a stray key over the address can be taken back.

**History and settings.** Pages you visit are remembered, so typing part of a site you've been to takes you back there. `⌘Y` lists the pages you've visited, newest first, and searches them. History > Clear History… forgets them all, and your recently closed tabs, after asking; deleting a space removes its history too. `⌘,` opens the settings in the command bar: pick the search engine (Google, DuckDuckGo, Bing, Brave Search, Kagi or Ecosia), turn ad blocking off for the current site, or make Map Browser your default browser.

**Today, pinned and favorites.**
- New tabs appear in **Today**, at the top of the list.
- `⌘D` pins a tab above the line. Right-click a tab to make it a **Favorite**, shown as a tile at the top.
- Drag tabs to reorder them or move them between sections.
- `⌘W`, the – or a middle click only puts a pinned tab or favorite to sleep; it stays in the sidebar with a faded icon. **Close Tab** in its right-click menu removes it, and **Go to Pinned Page** takes it back to the page you pinned.
- `⌘W`, the × or a middle click closes a Today tab, and `⇧⌘T` brings it back where it was. The library button at the bottom left lists recently closed tabs, also after you restart.
- **Clear** (`⇧⌘K`) closes all Today tabs at once. **Close Other Tabs** and **Close Tabs Below**, in a Today tab's right-click menu, close the rest or the ones below it. One `⇧⌘T` brings the whole lot back, in order.
- Double-click a tab, or right-click it and choose **Rename Tab**, to give it a name of your own. The same menu duplicates the tab, mutes it and copies its URL.
- To send a tab to another space, drag it onto that space's button, or right-click it and choose **Move to Space**. It opens there with that space's logins.

**Spaces.**
- Spaces appear as buttons at the bottom of the sidebar, each in its own colour or with its icon; the one you're in is raised. With many spaces, a … button lists them all.
- To switch, click a button, swipe sideways with two fingers on the sidebar, or use `⌃1`–`⌃9` or `⌥⌘←` / `⌥⌘→`, which go round from the last space to the first.
- Drag the buttons to put your spaces in the order you want.
- Double-click a space's name to rename it. Right-click the name, or use the … beside it, to set its colour and icon; try as many as you like before closing the menu. **Unload Space** puts all of its pages to sleep when you're done with them.
- The + at the bottom right creates a space; press Esc instead of naming it and it goes away again. **Delete Space**, in the same … menu or on a right-click on a space's button, removes a space with its tabs, logins and other site data, after asking. Your last space can't be deleted.

**Getting around.**

| Keys | Action |
|---|---|
| `⌘S` | Hide or show the sidebar |
| `⌘1`–`⌘9` | Go to a tab |
| `⌃Tab` | Back to the tab you were on. Hold `⌃` and press `Tab` again to go further back; the sidebar outlines the tab you'll get. `⌃Esc` cancels |
| `⌥⌘↑` / `⌥⌘↓` or `⇧⌘[` / `⇧⌘]` | Previous or next tab in the sidebar |
| `⌘R` | Reload |
| `⌘[` / `⌘]` or `⌘←` / `⌘→` | Back and forward |
| `⇧⌘C` | Copy the address |
| `⌘F` | Find on the page |
| `⌘+` / `⌘−` / `⌘0` | Zoom in, out, or back to actual size |
| `⌘P` | Print the page |
| `⌘Y` | Search your history |
| `⌘,` | Settings |

Help > Keyboard Shortcuts shows every shortcut in one panel. Web apps keep their own shortcuts: in an editor such as Figma, VS Code for the web or Google Docs, keys like `⌘Z`, `⌘A`, `⇧⌘K`, `⌘[` or `⌘F` do what the app says. Opening, closing and switching tabs and spaces always works.

Menus in the sidebar work from the keyboard too: `⇧F10` opens the selected tab's menu, the arrow keys move, Return chooses, → opens Move to Space, typing the start of a name jumps to it and Esc closes. When Map Browser asks before deleting, Return means Cancel.

Point at a link to see where it goes, at the bottom left of the page. Right-click a link or an image to open it in a new tab, save it to Downloads or copy its address, an image also to copy the picture, selected text to search for it, or the page to go back, reload or inspect it.

Drag the sidebar's edge to resize it; double-click the edge to reset it. With the sidebar hidden, rest the pointer on the window's left edge to peek at it; it hides again when you move away. Short messages such as "Copied URL" or the zoom level then appear briefly at the bottom left of the page, with a ✓ when something is done and a ✕ when it failed.

**The site menu.** The shield beside the address opens a menu for the site. Turn ad and tracker blocking off or on there, see which permissions the site was given or refused in this space and reset them, change the page zoom, or clear the site's cookies and stored data, which signs you out of it in this space only. Plain `http://` pages, and sites you went on to past a certificate warning, show **Not Secure** by the address. A page that isn't at 100 % shows its zoom level there; a click resets it.

**When a page needs you.**
- Sites and company proxies that ask for a username and password show a sign-in card on the page. Each card keeps what you typed while you look at other tabs, never shows it in another tab or space, and forgets it, password and all, once you answer or close the tab. Stop (⌘.) cancels it.
- A page with unsaved changes, such as a half-written comment, asks **Leave site?** before you close, move or reload it. Clearing Today, unloading a space and quitting never wait on a page.
- If a page crashes, runs out of memory or stops responding, the card says so and offers Reload (or Wait), and its tab shows a warning sign.
- A site whose security certificate doesn't check out shows a full warning with Back to Safety, which returns to the page before, or closes a tab opened just for that site. For servers on your Mac and devices you reach by their IP address you can still continue at your own risk; any other site may insist on a secure connection, so Map Browser never lets you past it there.
- When a page can't load, because you're offline, the address doesn't exist or the site refuses, the card says why in plain words, with Try Again.
- Tabs playing sound show a speaker; click it to mute the tab. Tabs using your camera or microphone show a red indicator.
- Blocked pop-ups say so, with Open to open the pop-up as a tab. A page going full screen shows its site and that Esc leaves full screen; ⌘T and ⌘L leave it too and open the command bar.

**Downloads.** Files go straight to your Downloads folder, numbered if a file with that name is already there. While they download, the library button at the bottom left shows their progress, or with the sidebar hidden, the bottom left of the page does. The library lists your recent downloads, also after you restart: × cancels one, ↻ downloads a failed or cancelled one again with its space's logins, and a click shows a finished one in Finder. A download that fails says why and marks the library button red. If files are still downloading when you quit, Map Browser asks first, as quitting cancels them.

**Camera, microphone and location.** Sites ask before using your camera, microphone, screen, location, notifications or other devices. The question appears in the sidebar, names exactly what the site wants and how long your answer lasts, and can be answered with Tab and Return. Your answer holds for that site in that space, for camera and microphone until you quit; screen sharing asks every time. The site menu lists your answers and resets them.

Sign-in and payment windows, such as "Continue with Google" or PayPal, open as small windows and finish signing you in on the page that asked; they can download files and ask for the camera like any tab. Email and phone links open Mail and FaceTime.

**From other apps.** Make Map Browser your default browser with Map Browser > Set as Default Browser…, and links you click in other apps open in it. Web pages, PDFs, images, text files and web locations (.webloc) open in Map Browser from Finder's Open With, or when dropped on its Dock icon or the sidebar, each in a new tab in the current space.

Spaces, tabs and selections come back when you reopen Map Browser, and the window opens where you left it, at the same size. If Map Browser can't save your tabs, for example because the disk is full, it says so until saving works again. If your saved tabs were damaged, it opens the last backup and tells you where it kept the damaged file. If your saved session can't be opened, for example after going back to an older version, Map Browser tells you why and leaves it untouched. Your browsing data, history included, stays on your Mac.

## Status

Map Browser is an early prototype for macOS. It has no extensions, split view, sync, bookmarks, password manager, private windows or tab folders yet, and it doesn't update itself. Battery life on par with Helium is the goal, but it hasn't been measured on a Mac yet.

## Credits

- Ad and tracker blocking uses the EasyList and EasyPrivacy filter lists by [The EasyList authors](https://easylist.to/), included under [CC BY-SA 3.0](licenses/EasyList-CC-BY-SA-3.0.txt), and Brave's [adblock-rust](licenses/adblock-rust-MPL-2.0.txt). The lists update with the app.
- Icons are from [Lucide](licenses/Lucide-ISC.txt).
- The text field is adapted from [GPUI](licenses/GPUI-Apache-2.0.txt).

Building from source: see [development notes](docs/development.md).
