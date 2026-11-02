import { ChevronDown, Hash, Headphones, Mic, Radio, Settings, Volume2 } from 'lucide-react'
import { useAppStore } from '../store/app-store'

const voiceMembers = ['Maya', 'Oskar', 'Tommy']

export function ChannelSidebar() {
  const server = useAppStore((state) =>
    state.servers.find((item) => item.id === state.selectedServerId),
  )

  return (
    <aside className="channel-sidebar">
      <button className="server-heading">
        <span>
          <strong>{server?.label ?? 'SocialSpeak'}</strong>
          <small>{server?.host ?? 'Choose a server'}</small>
        </span>
        <ChevronDown size={17} />
      </button>

      <div className="channel-scroll">
        <section className="channel-section">
          <p className="section-label">Text channels</p>
          <button className="channel-row active">
            <Hash size={17} />
            <span>general</span>
            <span className="unread-count">4</span>
          </button>
          <button className="channel-row">
            <Hash size={17} />
            <span>product</span>
          </button>
          <button className="channel-row">
            <Hash size={17} />
            <span>music-lounge</span>
          </button>
        </section>

        <section className="channel-section">
          <p className="section-label">Voice channels</p>
          <button className="channel-row voice-active">
            <Volume2 size={17} />
            <span>Studio</span>
            <Radio size={14} className="live-icon" />
          </button>
          <div className="voice-members">
            {voiceMembers.map((name, index) => (
              <div className="voice-member" key={name}>
                <span className={`mini-avatar avatar-${index + 1}`}>{name[0]}</span>
                <span>{name}</span>
                {index === 0 && <Mic size={13} />}
              </div>
            ))}
          </div>
          <button className="channel-row">
            <Volume2 size={17} />
            <span>Quiet room</span>
          </button>
        </section>
      </div>

      <div className="voice-panel">
        <div className="voice-state">
          <span className="signal-bars" aria-hidden="true">
            <i />
            <i />
            <i />
          </span>
          <span>
            <strong>Voice connected</strong>
            <small>Studio · QUIC 24 ms</small>
          </span>
        </div>
        <div className="voice-controls">
          <button className="icon-button" aria-label="Mute microphone">
            <Mic size={16} />
          </button>
          <button className="icon-button" aria-label="Deafen">
            <Headphones size={16} />
          </button>
          <button className="icon-button" aria-label="Voice settings">
            <Settings size={16} />
          </button>
        </div>
      </div>
    </aside>
  )
}
