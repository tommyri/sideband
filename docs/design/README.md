# Design notes

## Desktop prototypes

Start with the [desktop design system](../../DESIGN.md) for tokens, component states and layout behavior. Open the [editable Sideband workspace](Sideband%20workspace.figcraft) in FigCraft to inspect a component or try a flow. From a browser on GitHub, download the raw file and open it locally.

| FigCraft page | What it contains |
|---|---|
| Design system | **Foundations** for variables and text styles, **Component library** for workspace controls, and **Onboarding forms** for fields and actions. [Foundations preview](foundations.png) |
| Workspace prototypes | Desktop conversation, compact replies, voice rooms, settings and menus. [Compact call preview](compact-call-preview.png) |
| New user onboarding | Server discovery, account creation, login, certificate trust, recovery and the first message. [First server preview](onboarding-preview.png) |
| Profile and devices | Pair devices, choose sync servers, try an outage, link existing accounts, and recover a lost device. [Relay outage preview](profile-relay-outage-preview.png) |
| Interaction plan | Expected transitions, keyboard and microphone states, and acceptance checks for workspace and onboarding changes. |

On **Workspace prototypes**, choose **Present** and select a starting flow.

| Flow | What to try |
|---|---|
| Desktop · from chat to voice | Select Green Room, check who is inside, choose Join voice, then open #general with the call still active. |
| Compact · reply in a call | At 760 × 600, open Audio and return to the draft. Send the reply or leave voice and keep editing. |
| First visit · voice-only server | Preview an occupied or empty room and choose whether to join. |
| Host · invite a friend | Create a one-use invite and copy the complete invitation. |
| Dropped call · automatic return | Keep replying as chat reconnects and voice resumes in push-to-talk mode. |
| Dropped call · rejoin voice | Wait for chat, choose **Rejoin voice**, and finish the reply. |

The workspace boards cover the [compact call dock](https://github.com/tommyri/sideband/issues/35), [intentional voice entry](https://github.com/tommyri/sideband/issues/36), [server landing](https://github.com/tommyri/sideband/issues/37), [chat layout](https://github.com/tommyri/sideband/issues/38) and [personal audio settings](https://github.com/tommyri/sideband/issues/39).

For first-time setup, open **New user onboarding** and present **New user · join a server**. Select each field to fill it with sample data, create a server account, then send a first message in **#general**. **View room** opens a voice preview, and **Check audio first** offers an optional microphone check. The prototype simulates accounts, messages and audio.

The flow picker also includes a large-window walkthrough, self-signed certificate verification, an unreachable server, open registration, login-only servers and rejected-invite recovery. The **New member entry and recovery** board on **Interaction plan** describes validation, focus and server-specific behavior.

### Profile and devices concept

Open **Profile and devices → Present**. Use the flow menu at the top left to choose **My usual sync server is down**. Each paired desktop view shows two 760 × 600 app windows. Click the controls in the window labeled for the device you are using. Pairing links, credentials, account proofs, sync updates and outages are simulated. Clicking an empty field fills it with example data.

| Starting flow | What to try |
|---|---|
| My usual sync server is down | Join Tape Loop on Windows while Friday Games is down. At **Synced to 1 of 2 servers**, close Sideband on Windows. Open it on the Mac, enter Tape Loop and check sync activity. **Check again** simulates Friday Games coming back. [Outage preview](profile-relay-outage-preview.png) |
| Choose and manage sync servers | Choose Friday Games, add North Studio as a second server, then replace an offline host with Soundcheck. The replacement receives a current copy. **Manage backup** opens the separate recovery flow. |
| No joined server offers sync | Check availability again, open a host’s invitation, and choose the newly joined community for sync. |
| Both sync servers are offline | Join Tape Loop on Windows. The status shows that the change is saved on this device. **Check again** brings North Studio online and delivers the queued update. |
| Offer sync and preview the server logo | Toggle member sync and public logo visibility independently. Preview an invitation and the member’s picker with each setting on or off. Both identify the community by name and hostname. The public logo falls back to initials. Public server information omits the software build. |
| Link existing server accounts | Choose Friday Games, confirm the signed-in account, then check its Admin membership on Windows. **Review 8 other accounts** lets the user approve those memberships together. Night Shift keeps its per-device server login. [Account linking preview](profile-account-link-preview.png) |
| Link a Windows PC from a Mac | Choose **Link a device** on the trusted Mac. Open its pairing link on Windows or enter the sync server address and one-time code. Compare the phrases and allow or deny on the Mac. [Linking preview](profile-link-preview.png) |
| Sync an eleventh server | Join Tape Loop on Windows while the Mac sleeps. Open Sideband on the Mac and select Tape Loop to enter the same channel. North Studio stays listed while offline. [Server sync preview](profile-sync-preview.png) |
| Save a backup and its key | The walkthrough suggests a private iCloud Drive or Google Drive folder for the encrypted file and a password manager or paper for the recovery words. Someone who keeps the file as a password manager attachment can keep the words on paper. Choose **I saved my key** to see how to restore on a new computer. [Backup setup preview](profile-backup-preview.png) |
| Recover after losing the Mac | Choose the saved backup file, then enter the words that open that file. The help screen gives separate steps for a missing file and a missing key. After restoring, review devices and remove MacBook. North Studio reports pending removal until it reconnects. [Recovery preview](profile-recovery-preview.png) |
| Retry a recovery key | Select the invalid field to supply the corrected sample key, or open the help for a missing file or key. |
| Explore phone linking | Choose **Scan QR code** to simulate a scan, or enter the server address and code shown on the Mac. Compare the phrase and approve on MacBook. [Phone preview](profile-phone-preview.png) |

The **Profile portability and device access** and **Sync servers and existing memberships** boards on **Interaction plan** record the assumptions and acceptance checks. During the outage walkthrough, ask which server delivered the change, whether the PC still needs to be on, and what the user would need if they lost both computers.

**Could someone recover tomorrow?** Have a first-time user try **Save a backup and its key** with the example data. Then hide the screen and ask, “Your Mac is gone. What would you look for, where would you look, and what would you do on your new computer?” Listen for both the file and the words, kept in separate places they can still reach. Use **Recover after losing the Mac** to see whether they can follow through without coaching.

### Usability scenarios

The desktop app implements both flows. The reference designs are on **Workspace prototypes**. The **Invitations and interrupted calls** board on **Interaction plan** records the expected behavior and prompts for an observer.

**Invite a friend and see if they can send you a message.** Start the host at **Desktop · from chat to voice** to test whether they find **Invites** in the server menu. For a shorter session, start at **Host · invite a friend**. Try the default expiry and **Never**. **Copy invitation** simulates clipboard success. **Host · clipboard unavailable** shows the manual fallback. After the host chooses Copy, give the guest the displayed invitation and start them at **New user · join a server**. Its sample fields accept the same address and code. Watch whether the guest reaches their first message without asking the host for missing information. [Invitation preview](invitation-preview.png)

**Can anyone hear you? Has your reply been sent?** Ask at the moment the connection drops, when chat returns, and when the dock changes. Both dropped-call flows start with the same two-line reply to Mina. Wait about eight seconds for each simulated network change. Send becomes available as soon as chat returns. On the manual path, voice waits for **Rejoin voice**. Try **Leave voice** during recovery to cancel the return. The prototype models a previously active push-to-talk call. These timings are for the walkthrough. [Call recovery preview](call-recovery-preview.png)

For the implemented compact layout, the desktop browser test exercises Send, Leave voice and keyboard focus with a long room name and a two-line reply. [Run the compact browser test](../../apps/desktop/README.md#compact-call-browser-test)

### Updating the design

Edit shared variables, text styles and main components on **Design system**, then review their instances in the affected flows. Record changed values or usage in [DESIGN.md](../../DESIGN.md) and interaction changes on **Interaction plan**. Update the corresponding preview when its frame changes so reviewers can see the result in Git.

From the repository root, these commands inspect the saved file and reproduce the previews with FigCraft CLI 0.18.1.

```sh
workspace="docs/design/Sideband workspace.figcraft"
figcraft-cli info "$workspace"
figcraft-cli export "$workspace" --node 'Foundations' -o docs/design/foundations.png
figcraft-cli export "$workspace" --node 'Reply during a call · 760 × 600' -o docs/design/compact-call-preview.png
figcraft-cli export "$workspace" --node 'Join your first server' -o docs/design/onboarding-preview.png
figcraft-cli export "$workspace" --node 'Invitation ready to share' -o docs/design/invitation-preview.png
figcraft-cli export "$workspace" --node 'Chat restored · voice reconnecting' -o docs/design/call-recovery-preview.png
figcraft-cli export "$workspace" --node 'Link a computer · compare the phrase' -o docs/design/profile-link-preview.png
figcraft-cli export "$workspace" --node 'Sync servers · eleventh membership' -o docs/design/profile-sync-preview.png
figcraft-cli export "$workspace" --node 'Recovery · unlock backup' -o docs/design/profile-recovery-preview.png
figcraft-cli export "$workspace" --node 'MacBook · profile backup' -o docs/design/profile-backup-preview.png
figcraft-cli export "$workspace" --node 'Phone concept · scan or enter code' -o docs/design/profile-phone-preview.png
figcraft-cli export "$workspace" --node 'Relay outage · one copy delivered' -o docs/design/profile-relay-outage-preview.png
figcraft-cli export "$workspace" --node 'Existing accounts · confirm membership' -o docs/design/profile-account-link-preview.png
```

### Connect FigCraft to Codex over MCP

FigCraft CLI 0.18.1 includes a headless MCP server. On macOS, unzip the `figcraft-cli-0.18.1-macos-universal.zip` release asset and put `figcraft-cli` in a permanent directory on your `PATH`. You can also build the CLI from the tagged FigCraft source with `cargo build --release -p figcraft-cli`. [FigCraft 0.18.1 release](https://github.com/storytold/figcraft/releases/tag/v0.18.1) · [Upstream MCP guide](https://github.com/storytold/figcraft/blob/v0.18.1/docs/mcp.md)

Run these commands from the Sideband repository root. Codex records the absolute executable path and starts FigCraft over standard input and output. The write root lets the tools open the workspace, save design changes, and export previews under `docs/design`. For a read-only review, use `--automation-read-root` in the same command.

```sh
figcraft-cli --version
codex mcp add sideband-design -- "$(command -v figcraft-cli)" mcp \
  --automation-write-root "$PWD/docs/design"
codex mcp get sideband-design
```

Use this request in a new Codex chat for the checkout.

> Use the sideband-design MCP server to open `docs/design/Sideband workspace.figcraft` by absolute path with `open_file`. Call `inspect_document` with `{"depth":0,"fields":["name","type"]}` and report the document name and pages.

The result should be **Sideband — workspace design**, with **Workspace prototypes**, **Design system**, **Interaction plan**, **New user onboarding**, and **Profile and devices**. A new headless session starts with an untitled file, so open the repository file at the start of each session. After editing, call `save_file` on the opened workspace and regenerate the affected preview with the CLI commands above.

## Technical design notes

The [project plan](../plan/00-overview.md) sets the order of implementation.
These notes record the underlying technical decisions, especially the voice codec.

| Document | What it settles |
|---|---|
| [decisions.md](decisions.md) | Numbered decisions D1–D12, with the constraints later tasks must keep |
| [voice.md](voice.md) | Opus profiles, encoder contract, processing, devices, latency budget |

The wire layout is in `docs/plan/02-protocol.md`. If a number there and a
number here disagree, the protocol document wins for bytes on the wire and
`voice.md` wins for encoder behavior. Update both in the same change.

The profile numbers are implemented as `VoiceProfile::encoder_params` in
`ss-core`, merged into `main` with T-003. Keep the code, these documents, and
the protocol table in agreement.

T-018 (the listening test against TeamSpeak 3) may revise bitrates, frame
size, and the jitter target. Until that note exists, `voice.md` is what
implementers code against.
