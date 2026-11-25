# User guide

How to use the Sideband desktop app: joining a server, talking, setting up
audio and push-to-talk, and, for administrators, roles and moderation.

To install the app, see [testing builds](testing-app.md#install-and-identify).
To report a problem with evidence, see
[guided tests](testing-app.md#connecting-and-reporting).

> **What has been checked.** Every feature here is implemented and covered by
> automated tests. The hands-on checks on real machines are still open for
> most of them, on every operating system; the
> [acceptance matrix](plan/tasks/ACCEPTANCE.md) lists each one. Steps marked
> **(unverified)** below describe what the code does on that platform, but
> nobody has yet confirmed them on a real Windows or Linux machine.

## Add a server

1. On first launch, **Join your first server** opens with the address field focused.
   To join another server later, choose **Add server** in the server rail.
2. Paste the address your host shared and choose **Find server**. Sideband accepts
   `host`, `host:port`, `ss://host:port`, and `[IPv6 address]:port`. The default
   port is 443. If your host gave you a separate HTTPS port, enter it under
   **Advanced connection options**. You can edit the address and retry a failed check.
3. A verified domain or previously trusted fingerprint takes you to the account
   form. For a new self-signed certificate, compare the full SHA-256 fingerprint
   with your host through a conversation you already trust. Confirm that it
   matches and choose **Trust this server** to continue. The join screen shows
   the server name and hostname with its public logo or initials.
4. On a server that offers profile sign-in, choose **Create a local profile**
   or **Join with profile**. If you already have a username and password on
   this server, choose **Use a server account** and log in to keep your
   messages and roles. On older servers, create an account or log in. Usernames
   are 3–32 lowercase letters, digits, underscores or dots. Passwords are
   8–128 UTF-8 bytes. Invite-only servers also ask for the code from your host.
   Servers with closed registration offer login. A rejected attempt keeps your
   username and clears the password. Once connected, you can send a message
   before setting up your microphone.

The app remembers your session on this computer, never your password.
Next time, select the server and choose **Reconnect / log in**, then
**Reconnect with saved session**. While the app is open it reconnects by
itself after a network drop.

### Use a local profile on Mac or Windows

Choose **Create a local profile** in **Add server** to name your profile before
you pick a server. You can also do this while accepting an invitation. Sideband
keeps the sign-in keys in your Mac or Windows credential store. You can use the
same profile on this computer to join several communities without creating a
password for each one.

When you choose **Join with profile**, check the username others will see on
this server. You can pick a different username for each community. Enter your
host’s invite code if the server asks for one. On your next visit, use your
saved session or choose **Use a local profile** to sign in again. Your roles
and messages belong to your membership on that server.

Keep this installation and its credential store if you need to return with the
same profile. Pairing another computer, linking an existing password account
and restoring a backup are planned separately.

### After joining

Sideband reopens the last text channel or direct message you viewed with this
account on this server. On your first visit, or if that conversation is gone,
it opens **#general** or the first text channel you can read. If there are
only voice rooms, the lobby shows who’s there. Select a room to preview it,
or choose **Join voice** to enter the call. If you have no accessible channels
or conversations, you can switch servers from the same screen.

If the app later says **Server certificate changed**, stop and ask the owner.
It can mean the server was reinstalled, or that someone is intercepting your
connection. Replace the trusted certificate only after the owner confirms the
new fingerprint. Why this matters:
[security §3](security.md#3-tofu-what-it-protects-and-what-it-does-not).

**Remove server** (on the server's home page, under **Manage server**) signs
the session out on the server and forgets it on this computer. It does not
delete your account. **Move earlier** and **Move later** reorder the server
rail.

## Talk

Select a voice room to see who’s there and how your saved talking mode will work.
Choose **Join voice** when you’re ready. You can browse other rooms and text
channels during a call. Choosing **Join voice** in another room switches the call there.

Open **Personal settings** at the bottom of the channel list or in the compact
channel header. During a call, **Audio** in the dock opens the same preferences.

- **Push-to-talk** (the default): hold **Hold to talk** with the mouse, or
  press Space or Enter while it has focus, or hold your push-to-talk key
  (below). Releasing, the window losing focus, muting or deafening stops
  transmission.
- **Voice activation**: you transmit while your microphone is above the
  activation threshold.
- **Open microphone**: you transmit continuously while connected and unmuted.

Mute and deafen apply in every mode. **Leave voice** in the call dock ends
the call and keeps you connected to the server. To disconnect, select the
server name above the channel list and choose **Disconnect from server**.
The latency figure in the call controls
is network round-trip time, not the delay from mouth to ear.

Click or right-click a member for per-person volume, **Mute for me**,
copying their user ID, and the moderation actions your role allows. Closing
the window hides Sideband to the tray; choose **Quit** in the tray menu to
stop it. The tray menu also has **Mute microphone** and **Deafen**.

## Audio settings

Choose your microphone, output device and talking mode in **Personal settings**.
**Apply settings** saves changes and updates an active call. **Done** or Escape
returns you to the conversation. **Cancel** discards unapplied changes.

### Devices

- **Microphone** and **Output device**: **System default** follows the
  operating system's choice. Open **More audio controls** and choose **Refresh devices** after plugging something in.
- **Microphone input**: for audio interfaces with several inputs, pick the one
  your microphone is plugged into. **Average of all inputs** lowers a single
  microphone's level and adds the noise of the unused inputs.
- **Check mic** plays your raw microphone back for 10 seconds after a one-second
  delay; use headphones. **Test output** plays a sound. Tests run only on this
  computer, only outside a call, and send nothing to the server.

The microphone permission state is shown. If it is denied, allow Sideband in
your system's privacy settings: on macOS **System Settings → Privacy &
Security → Microphone**; on Windows **Settings → Privacy & security →
Microphone** **(unverified)**.

### Transmission

Open **More audio controls** for the settings specific to your talking mode.

| Setting | Mode | What it does | Default |
|---|---|---|---|
| **Release delay (ms)** | Push-to-talk | Keeps transmitting this long after you release the key, so the ends of words are not cut | 150 |
| **Capture in-app binding** | Push-to-talk | A key or mouse button that works while Sideband is focused, outside text fields and dialogs | none |
| **Activation threshold (dBFS)** | Voice activation | Level above which you transmit | −40 |
| **Quiet hangover (ms)** | Voice activation | How long transmission stays open after you go quiet | 300 |
| **Auto-calibrate** | Voice activation | Listens to 2 seconds of background noise and sets the threshold 12 dB above it; leave voice first | |

### Processing

Expand **Advanced processing** to configure these options.

All processing is **off by default**, because each stage adds delay or CPU
work and none of it is needed with a good headset. The software stages share
a budget of 3 ms of processing per 10 ms audio frame.

| Option | What it does | What it costs |
|---|---|---|
| **Echo cancellation** | Removes your speakers' sound from your microphone. Use it when you are not on headphones | CPU work, up to 1.5 ms per frame |
| **Noise suppression: WebRTC** | Reduces steady background noise (fans, hum) | About 7 ms added latency |
| **Noise suppression: RNNoise** | A neural network suppressor, better on irregular noise such as keyboards | 10 ms added latency |
| **Automatic gain** | Evens out your microphone volume | CPU work, up to 1.5 ms per frame |
| **Use Apple voice processing** (macOS only) | The system's own echo cancellation, noise suppression and gain. Uses the system default devices and turns the software stages off | Device latency varies |

How the processing chain is built and why it is opt-in:
[voice design](design/voice.md#processing-chain).

### Output and advanced

These options are in **More audio controls**.

- **Master volume** (0 to 200 %), **Cue volume** for join/leave and other
  sounds, and **Reduce other voices while speaking** (0 to 50 %).
- **People in this server** (during a call): per-person volume, and **Reset
  all per-user volumes**.
- **Minimum** and **Maximum jitter buffer** (20 to 200 ms): more buffering
  rides out an unsteady network at the cost of delay. Changes take effect at
  the next pause in speech. The channel's profile and current bitrate are
  shown here during a call.
- **Diagnostics** (Ctrl+Shift+D): live network and audio statistics, and a
  redacted log bundle to share. See
  [live diagnostics](testing-app.md#live-diagnostics).

## Push-to-talk outside the app

The in-app binding only works while Sideband is focused. For push-to-talk,
mute and deafen while a game or another app is in front, open **Personal
settings → More audio controls → Global shortcuts**:

1. **Capture push-to-talk** (or mute, or deafen), then press and release the
   key or mouse button. Capture times out after 15 seconds; **Cancel capture**
   stops it.
2. **Save shortcuts.** Nothing listens until you save a binding. If two
   actions share a trigger the app refuses to save.
3. Check **Platform status**. It says whether the listener is running and
   lists what this platform cannot do.

Global push-to-talk applies when the talking mode is push-to-talk; it does not
turn voice activation or open microphone off. Typing in a password field, or
capturing a new binding, suspends the listener and releases push-to-talk at
once. **Recent input events** shows the last ten shortcut actions on this
computer only, never typed text. After fixing a permission, choose **Retry
saved bindings**.

The shortcuts listen to the keyboard and mouse; they do not take the key away
from the focused application, so the game still sees it
([decision D8](design/decisions.md#d8--push-to-talk-listens-it-does-not-register-a-hotkey)).

### Windows **(unverified)**

No permission is needed. Keys and Mouse4/Mouse5 work while another
application is in front, and left and right modifiers are told apart. Sideband
cannot see input while a game running as administrator is in front, unless
Sideband runs as administrator too. The status panel says so when it
detects this. Not yet checked in a fullscreen game
([T-048](plan/tasks/T-048-input-windows.md)).

### macOS

Global shortcuts need **Input Monitoring**. The app asks when you capture or
first start listening; you can also grant it in **System Settings → Privacy &
Security → Input Monitoring**, or with **Open system shortcut
permissions/settings**. Grant it only if you want global shortcuts. Test
builds are ad-hoc signed, so a new build can need the permission granted
again. If you revoke the permission, or a password field takes secure input
while you hold the key, push-to-talk is released. The game and permission
checks are still open ([T-049](plan/tasks/T-049-input-macos.md)).

### Linux **(unverified)**

What works depends on the session:

- **X11**: keys and mouse buttons are captured like on the other platforms;
  no permission is needed.
- **Wayland** (KDE, GNOME 48 and later, Hyprland): the desktop's global
  shortcuts portal owns the keys. Tick **Enable system-managed PTT, mute and
  deafen**, save, and then choose the keys in your desktop's settings; the
  app cannot capture them and mouse buttons are not available. KDE and GNOME
  may keep the key from reaching the game. Hyprland passes it through only
  with a `bindn` binding.
- **Wayland without the portal** (Sway, for example): global shortcuts are
  unavailable. Use voice activation, or opt in to **Allow evdev fallback**.
  evdev reads every key in every application and needs access to your input
  devices (usually membership of the `input` group), which you must set up
  yourself; the app never changes system permissions.

The desktop-by-desktop details are in
[Linux input](../crates/ss-input/linux.md); the checks are open in
[T-050](plan/tasks/T-050-input-linux.md).

## Keyboard shortcuts

`Ctrl/Cmd` means Ctrl on Windows and Linux, Cmd on macOS.

| Keys | Where | Action |
|---|---|---|
| Ctrl/Cmd+Shift+M | In a call, outside text fields and dialogs | Toggle mute |
| Ctrl/Cmd+Shift+Alt+D | In a call, outside text fields and dialogs | Toggle deafen |
| Ctrl/Cmd+Shift+Up / Down | Outside text fields and dialogs | Previous or next channel |
| Ctrl+Shift+D | Anywhere (Ctrl on macOS too) | Open or close diagnostics |
| Space or Enter (held) | **Hold to talk** focused | Push-to-talk |
| Enter / Shift+Enter | Message box | Send / new line |
| Ctrl/Cmd+B, I, E | Message box | Bold, italic, code |
| Up | Empty message box | Edit your last message |
| Escape | Message box | Cancel the reply or edit, or close suggestions |
| Up / Down, then Enter or Tab | Mention and emoji suggestions | Choose a suggestion |
| Up / Down, Page Up / Page Down, Home / End | Message history | Move between messages |
| Enter | A message in the history | Focus its actions |
| Up / Down, Home / End | Server or message menu | Move between available actions |
| Escape | Server or message menu | Return to the server menu button or the originating message |
| Escape | Message history | Back to the message box |

Global shortcuts (previous section) are yours to choose; the app sets none.

## Chat, messages and notifications

Text channels and direct messages support Markdown, mentions, replies, edits,
attachments and link previews. **Search messages** in the channel header
searches text channels and your DMs; see [message search](message-search.md).

Hover over a message or focus it in the history to reveal **Reply**. Open **More actions** (the
three dots beside **Reply**) to copy its text, link, or ID. **Edit** and
**Delete** appear there when available.

Choose **Members** in the conversation header to see the server roster or
the participants in a direct message. Press Escape or choose **Close members**
to return to the Members button. Your draft and reply stay in place. In a
compact window, **Channels** opens the channel list and server rail.

The bell in the server rail opens **Notification settings**: **Do not
disturb** (silences sounds and desktop notifications and shows you as Do not
disturb), desktop notifications while Sideband is in the background, whether
they show message text (off by default), and **Mute this server**, and a level per text
channel and DM (**All messages**, **Mentions only**, **None**; channels
default to mentions). Do not disturb and a muted server override every
channel. Your operating system can still block notifications.

**App updates** in the server rail checks for a newer version on builds with
signed updates. Updating restarts Sideband, so leave voice first. Other
builds update by installing a newer download; on Linux only the AppImage
updates itself.

## For administrators

Select the server name above the channel list to open its menu. Your roles
determine which administrative actions appear.

| Menu action | Needs | What you do there |
|---|---|---|
| **Invites** | Create invites; Manage server to see and revoke everyone's | Create invites with a use limit and expiry; revoke them |
| **Members & moderation** | Kick members or Ban members | Search members, kick, ban for a time or permanently, unban |
| **Roles and permissions** | Manage roles or Manage channels | Create, edit, reorder and delete roles; assign roles to members; set per-channel overrides |
| **Manage channels** | Manage channels | Create, edit, move and delete channels and categories |
| **Server settings** | Manage server | Name, description, icon, **Show logo before people join**, and registration (invite only or closed) |
| **Webhooks** | Manage server | Incoming webhooks that let another service post into a channel |
| **Audit log** | View audit log | Filtered history of administrative actions |

Moderation of a person in voice (server mute, move to another channel,
disconnect, ban) is also in that member's menu.

### How roles work

- Every member has the **@everyone** role. By default it can view channels,
  send messages, attach files, join voice, speak, share video, create invites
  and change their own nickname.
- The first account on a server gets the seeded **Admin** role, which holds
  **Administrator**: it bypasses every permission, channel override and role
  order. Admin cannot be deleted. Administrators are peers; there is no owner
  above them.
- Roles are ordered. You can act on, and assign, only members and roles below
  your highest role, and you can grant only permissions you hold yourself.
- **Channel overrides** allow or deny a permission for a role in one channel,
  for example a private channel that @everyone cannot view.
- Several permissions depend on others: speaking needs Connect and View
  channel; attaching files needs Send messages. The role editor explains each
  permission.

The exact resolution rules are in the
[data model](plan/03-data-model.md), and how the server enforces them is in
[security §5](security.md#5-authorization). A server operator can grant Admin
from the command line; see
[self-hosting](self-hosting.md#running-server-commands).
