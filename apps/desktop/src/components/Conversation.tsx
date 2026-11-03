import { Bell, Bug, Hash, HelpCircle, Moon, Pin, Search, Smile, Sun } from 'lucide-react'
import { useAppStore } from '../store/app-store'

const messages = [
  {
    name: 'Maya Chen',
    time: '09:42',
    avatar: 'MC',
    color: 'avatar-1',
    body: 'Morning! The native bridge is alive. Events are arriving in order and reconnects keep the shell responsive.',
    reaction: '⚡ 3',
  },
  {
    name: 'Oskar Lind',
    time: '09:47',
    avatar: 'OL',
    color: 'avatar-2',
    body: 'Nice. I pushed a tiny server-side change for the local test path. Certificate pinning still catches first use as expected.',
  },
  {
    name: 'Alex Rivera',
    time: '09:53',
    avatar: 'AR',
    color: 'avatar-3',
    body: 'The app shell is taking shape now — compact, calm, and all network traffic stays behind Rust.',
    reaction: '✓ 2',
  },
]

export function Conversation() {
  const toggleDebug = useAppStore((state) => state.toggleDebug)
  const theme = useAppStore((state) => state.theme)
  const toggleTheme = useAppStore((state) => state.toggleTheme)

  return (
    <main className="conversation">
      <header className="conversation-header">
        <div className="channel-title">
          <Hash size={20} />
          <span>
            <strong>general</strong>
            <small>Team updates and everyday conversation</small>
          </span>
        </div>
        <div className="header-actions">
          <button className="icon-button" aria-label="Notifications">
            <Bell size={17} />
          </button>
          <button className="icon-button" aria-label="Pinned messages">
            <Pin size={17} />
          </button>
          <label className="search-box">
            <Search size={15} />
            <input aria-label="Search messages" placeholder="Search" />
            <kbd>⌘ K</kbd>
          </label>
          <button className="icon-button" onClick={toggleTheme} aria-label="Toggle theme">
            {theme === 'dark' ? <Sun size={17} /> : <Moon size={17} />}
          </button>
          <button
            className="icon-button debug-trigger"
            onClick={toggleDebug}
            aria-label="Toggle debug panel"
          >
            <Bug size={17} />
          </button>
          <button className="icon-button" aria-label="Help">
            <HelpCircle size={17} />
          </button>
        </div>
      </header>

      <div className="message-scroll">
        <div className="channel-intro">
          <div className="intro-icon">
            <Hash size={25} />
          </div>
          <h1>Welcome to #general</h1>
          <p>
            This is the beginning of this channel. A quiet place to keep the whole team aligned.
          </p>
        </div>
        <div className="date-divider">
          <span>Today</span>
        </div>
        {messages.map((message) => (
          <article className="message" key={`${message.name}-${message.time}`}>
            <span className={`message-avatar ${message.color}`}>{message.avatar}</span>
            <div>
              <div className="message-meta">
                <strong>{message.name}</strong>
                <time>{message.time}</time>
              </div>
              <p>{message.body}</p>
              {message.reaction && <button className="reaction">{message.reaction}</button>}
            </div>
          </article>
        ))}
      </div>

      <div className="composer-wrap">
        <div className="composer">
          <button aria-label="Add attachment" className="composer-add">
            +
          </button>
          <textarea aria-label="Message general" placeholder="Message #general" rows={1} />
          <button aria-label="Choose emoji" className="icon-button">
            <Smile size={18} />
          </button>
        </div>
        <span className="typing-note">Maya is typing…</span>
      </div>
    </main>
  )
}
