// Playwright runs this function inside each fresh page, in place of the native bridge.
export function connectRoom(options) {
  const { roomName, extraServers = 0 } =
    typeof options === 'string' ? { roomName: options } : options
  let inVoice = true
  let callbackId = 0
  let ready = true
  let muted = false
  const callbacks = new Map()
  const listeners = new Map()
  const emit = (event, payload) => {
    for (const [id, name] of listeners) {
      if (name === event) callbacks.get(id)?.({ event, id, payload })
    }
  }
  const server = {
    id: 'night',
    label: 'Night Shift',
    host: 'nightshift.example',
    port: 443,
    httpsPort: 443,
    status: { kind: 'connected' },
    unread: 0,
  }
  const channel = {
    parentId: null,
    profile: 'voice',
    unread: false,
    mentions: 0,
    canJoin: true,
    canSpeak: true,
    userLimit: null,
  }
  const channels = [
    { ...channel, id: 'general', name: 'general', kind: 2 },
    { ...channel, id: 'voice', name: roomName, kind: 3 },
  ]
  const message = {
    id: '01',
    content: 'Who can bring a recorder?',
    channelId: 'general',
    authorId: 'mina',
    username: 'mina',
    authorName: 'Mina',
    createdAt: Date.now(),
    editedAt: null,
    deleted: false,
    replyTo: null,
    attachmentCount: 0,
    attachments: [],
    previews: [],
  }
  const messages = [
    message,
    {
      ...message,
      id: '02',
      authorId: 'owen',
      authorName: 'Owen',
      username: 'owen',
      content: 'I can bring the spare batteries.',
    },
  ]
  window.compactCall = { messagesSent: [], leaves: 0 }
  window.roomConnection = (kind, voice) => {
    ready = voice === 'seated'
    inVoice = voice !== 'ended'
    server.status = kind === 'reconnecting' ? { kind, attempt: 1, delayMs: 1000 } : { kind }
    emit('ss://event', {
      serverId: server.id,
      event: { kind: 'connection', status: server.status },
    })
  }
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener(_event, id) {
      listeners.delete(id)
    },
  }
  window.__TAURI_INTERNALS__ = {
    transformCallback: (callback) => {
      callbacks.set(++callbackId, callback)
      return callbackId
    },
    invoke: async (command, args) => {
      switch (command) {
        case 'plugin:event|listen':
          listeners.set(args.handler, args.event)
          return args.handler
        case 'get_snapshot':
          return {
            servers: [
              server,
              ...Array.from({ length: extraServers }, (_, i) => ({
                ...server,
                id: `server-${i}`,
                label: `Server ${i}`,
                status: { kind: 'disconnected' },
              })),
            ],
          }
        case 'get_server_view':
          return {
            ownUserId: 'jules',
            channels,
            directMessages: [],
            moreDirectMessages: false,
            members: [],
            participants: inVoice
              ? [
                  {
                    userId: 'jules',
                    connectionId: 'local',
                    channelId: 'voice',
                    muted: false,
                    deafened: false,
                    serverMuted: false,
                  },
                ]
              : [],
            canModerateMembers: false,
            canManageServer: false,
            canManageChannels: false,
            canManageRoles: false,
            canManageWebhooks: false,
            canViewAuditLog: false,
            canCreateInvites: true,
            canManageInvites: false,
            activeVoice: inVoice
              ? { serverId: server.id, channelId: 'voice', muted, deafened: false, ready }
              : null,
          }
        case 'invite_page':
          return []
        case 'create_server_invite':
          return {
            id: 'test-invite',
            code: 'night-shift-test',
            expiresAt: Date.UTC(2026, 10, 26, 18),
          }
        case 'chat_snapshot':
          return {
            messages,
            ownUserId: 'jules',
            lastRead: '02',
            canSend: true,
            canAttach: true,
            canManage: false,
            start: true,
            latest: true,
          }
        case 'load_history':
          return []
        case 'screen_snapshot':
          return { canShare: true, tracks: [], share: null, watching: [] }
        case 'audio_environment':
          return {
            preferences: { version: 1, mode: 'ptt', binding: 'Space' },
            devices: [
              { id: 'mic', name: 'Test microphone', input: true, output: false, inputChannels: 1 },
            ],
            defaultInput: 'mic',
            apple: false,
            mouse: false,
            systemBinding: false,
            permission: null,
            microphone: 'Granted',
            limitations: [],
            rnnoise: true,
            sonora: false,
          }
        case 'audio_level':
          return -40
        case 'send_message':
          window.compactCall.messagesSent.push(args.input)
          messages.push({
            ...message,
            id: '03',
            authorId: 'jules',
            authorName: 'Jules',
            username: 'jules',
            content: args.input.content,
            replyTo: args.input.replyTo,
          })
          return '03'
        case 'voice_control':
          if (args.action === 'mute') muted = args.enabled
          return
        case 'join_voice':
          window.rejoinRequest = args
          inVoice = true
          ready = true
          muted = args.startMuted ?? false
          return
        case 'leave_voice':
          inVoice = false
          window.compactCall.leaves++
          return
        default:
          return null
      }
    },
  }
}
