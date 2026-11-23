# Design notes

## Desktop prototypes

Start with the [desktop design system](../../DESIGN.md) for tokens, component states and layout behavior. Open the [editable Sideband workspace](Sideband%20workspace.figcraft) in FigCraft to inspect a component or try a flow. From a browser on GitHub, download the raw file and open it locally.

| FigCraft page | What it contains |
|---|---|
| Design system | **Foundations** for variables and text styles, **Component library** for workspace controls, and **Onboarding forms** for fields and actions. [Foundations preview](foundations.png) |
| Workspace prototypes | Desktop conversation, compact replies, voice rooms, settings and menus. [Compact call preview](compact-call-preview.png) |
| New user onboarding | Server discovery, account creation, login, certificate trust, recovery and the first message. [First server preview](onboarding-preview.png) |
| Interaction plan | Expected transitions, keyboard and microphone states, and acceptance checks for workspace and onboarding changes. |

On **Workspace prototypes**, choose **Present** and select a starting flow.

| Flow | What to try |
|---|---|
| Desktop · from chat to voice | Select Green Room, check who is inside, choose Join voice, then open #general with the call still active. |
| Compact · reply in a call | At 760 × 600, open Audio and return to the draft. Send the reply or leave voice and keep editing. |
| First visit · voice-only server | Preview an occupied or empty room and choose whether to join. |

The workspace boards cover the [compact call dock](https://github.com/tommyri/sideband/issues/35), [intentional voice entry](https://github.com/tommyri/sideband/issues/36), [server landing](https://github.com/tommyri/sideband/issues/37), [chat layout](https://github.com/tommyri/sideband/issues/38) and [personal audio settings](https://github.com/tommyri/sideband/issues/39).

For first-time setup, open **New user onboarding** and present **New user · join a server**. Select each field to fill it with sample data, create a server account, then send a first message in **#general**. **View room** opens a voice preview, and **Check audio first** offers an optional microphone check. The prototype simulates accounts, messages and audio.

The flow picker also includes a large-window walkthrough, self-signed certificate verification, an unreachable server, open registration, login-only servers and rejected-invite recovery. The **New member entry and recovery** board on **Interaction plan** describes validation, focus and server-specific behavior.

### Updating the design

Edit shared variables, text styles and main components on **Design system**, then review their instances in the affected flows. Record changed values or usage in [DESIGN.md](../../DESIGN.md) and interaction changes on **Interaction plan**. Update the corresponding preview when its frame changes so reviewers can see the result in Git.

From the repository root, these commands inspect the saved file and reproduce the three previews with FigCraft CLI 0.18.1.

```sh
workspace="docs/design/Sideband workspace.figcraft"
figcraft-cli info "$workspace"
figcraft-cli export "$workspace" --node 'Foundations' -o docs/design/foundations.png
figcraft-cli export "$workspace" --node 'Reply during a call · 760 × 600' -o docs/design/compact-call-preview.png
figcraft-cli export "$workspace" --node 'Join your first server' -o docs/design/onboarding-preview.png
```

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
