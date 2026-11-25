import { useEffect, useRef, useState, type FormEvent } from 'react'
import { ArrowLeft, ChevronRight, X } from 'lucide-react'
import { getSnapshot } from '../../lib/api'
import { useAppStore } from '../../store/app-store'
import { createLocalProfile, getLocalProfile } from '../onboarding/localProfile'
import { friendlyError } from '../onboarding/validation'
import { AccountLinking } from './AccountLinking'
import { SyncServers } from './SyncServers'
import { ProfileBackup } from './ProfileBackup'
import { DevicePairing } from './pairing/DevicePairing'
import './profile-settings.css'

type Section = 'home' | 'accounts' | 'sync' | 'backup' | 'restore' | 'pair' | 'join'

const destinations = [
  {
    section: 'pair',
    title: 'Link a device',
    hint: 'Use your profile and server memberships on another computer.',
  },
  {
    section: 'accounts',
    title: 'Link existing accounts',
    hint: 'Keep the messages and roles you already have on each server.',
  },
  {
    section: 'sync',
    title: 'Sync servers',
    hint: 'Choose a community to store an encrypted copy of your profile.',
  },
  {
    section: 'backup',
    title: 'Manage backup',
    hint: 'Save a file and recovery words in case you lose this computer.',
  },
] as const

export function ProfileSettings({
  initialSection = 'home',
  invitation,
  close,
  onRestored,
}: {
  initialSection?: 'home' | 'restore' | 'join'
  invitation?: string
  close: () => void
  onRestored?: (name: string) => void
}) {
  const [section, setSection] = useState<Section>(initialSection)
  const [name, setName] = useState<string | null>()
  const [draft, setDraft] = useState('')
  const [pending, setPending] = useState(false)
  const [installing, setInstalling] = useState(false)
  const [error, setError] = useState('')
  const dialog = useRef<HTMLDialogElement>(null)
  const heading = useRef<HTMLHeadingElement>(null)
  const lastSection = useRef<Section>(initialSection)
  useEffect(() => {
    const trigger = document.activeElement
    const modal = dialog.current
    let active = true
    modal?.showModal()
    heading.current?.focus()
    void getLocalProfile()
      .then((profile) => {
        if (active) setName(profile)
      })
      .catch((e: unknown) => {
        if (active) setError(friendlyError(e))
      })
    return () => {
      active = false
      modal?.close()
      if (trigger instanceof HTMLElement && trigger.isConnected) trigger.focus()
    }
  }, [])
  useEffect(() => {
    if (lastSection.current === section) return
    if (section === 'home') {
      const trigger = dialog.current?.querySelector<HTMLButtonElement>(
        `[data-destination="${lastSection.current}"]`,
      )
      const target = trigger ?? heading.current
      target?.focus()
    } else heading.current?.focus()
    lastSection.current = section
  }, [section])
  async function create(event: FormEvent) {
    event.preventDefault()
    setPending(true)
    setError('')
    try {
      setName(await createLocalProfile(draft.trim()))
    } catch (e) {
      setError(friendlyError(e))
    } finally {
      setPending(false)
    }
  }
  async function restored(profileName: string, dismiss = false) {
    setName(profileName)
    try {
      const snapshot = await getSnapshot()
      useAppStore.getState().hydrate(snapshot.servers)
    } catch (e) {
      setError(friendlyError(e))
      return
    }
    if (onRestored) onRestored(profileName)
    else if (dismiss) close()
    else setSection('home')
  }
  async function dismiss(back = false) {
    if (installing) return
    if (section === 'restore' || (section === 'join' && name === null)) {
      // Closing the success screen should also reveal restored memberships.
      try {
        const current = await getLocalProfile()
        if (current) {
          await restored(current, !back)
          return
        }
      } catch (e) {
        setError(friendlyError(e))
      }
    }
    if (back) setSection('home')
    else close()
  }
  return (
    <dialog
      className="profile-settings"
      ref={dialog}
      aria-labelledby="profile-settings-title"
      onCancel={(e) => {
        e.preventDefault()
        e.stopPropagation()
        void dismiss()
      }}
    >
      <header>
        {section !== 'home' && initialSection === 'home' && (
          <button
            type="button"
            className="profile-back"
            aria-label="Back to profile and devices"
            disabled={installing}
            onClick={() => void dismiss(true)}
          >
            <ArrowLeft size={18} />
          </button>
        )}
        <h2 ref={heading} id="profile-settings-title" tabIndex={-1}>
          Profile and devices
        </h2>
        <button
          type="button"
          className="profile-close"
          disabled={installing}
          aria-label="Close profile settings"
          onClick={() => void dismiss()}
        >
          <X size={19} />
        </button>
      </header>
      <div className="profile-settings-content">
        {error && <p role="alert">{error}</p>}
        {section === 'restore' && (
          <ProfileBackup
            initialMode="restore"
            onCancel={initialSection === 'restore' ? close : () => setSection('home')}
            onDone={(value) => void restored(value)}
          />
        )}
        {section === 'home' && name === undefined && !error && (
          <p role="status">Opening your profile…</p>
        )}
        {section === 'home' && name === null && (
          <form onSubmit={(e) => void create(e)}>
            <p>Create a local profile to bring your server accounts together.</p>
            <label>
              Profile name
              <input
                autoComplete="off"
                required
                maxLength={64}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
              />
            </label>
            <div className="profile-actions">
              <button className="button primary" disabled={pending || !draft.trim()}>
                Create profile
              </button>
              <button
                className="button secondary"
                type="button"
                data-destination="join"
                onClick={() => setSection('join')}
              >
                Link this computer
              </button>
              <button
                className="button secondary"
                type="button"
                data-destination="restore"
                onClick={() => setSection('restore')}
              >
                Restore from backup
              </button>
            </div>
          </form>
        )}
        {section === 'home' && name && (
          <>
            <p className="profile-owner">
              Your profile <strong>{name}</strong>
            </p>
            <nav aria-label="Profile settings">
              {destinations.map((item) => (
                <button
                  key={item.section}
                  data-destination={item.section}
                  type="button"
                  onClick={() => setSection(item.section)}
                >
                  <span>
                    <strong>{item.title}</strong>
                    <small>{item.hint}</small>
                  </span>
                  <ChevronRight size={18} />
                </button>
              ))}
            </nav>
          </>
        )}
        {section === 'join' && name === undefined && !error && (
          <p role="status">Opening your profile…</p>
        )}
        {section === 'join' && name && (
          <p>
            This computer already has a profile. Open this invitation on a computer you want to add.
          </p>
        )}
        {(section === 'pair' || (section === 'join' && name === null)) && (
          <DevicePairing
            source={section === 'pair'}
            invitation={invitation}
            onInstalling={setInstalling}
            onChooseRelay={() => setSection('sync')}
            onClose={close}
            onLinked={(value) => void restored(value)}
          />
        )}
        {section === 'accounts' && <AccountLinking />}
        {section === 'sync' && <SyncServers onManageBackup={() => setSection('backup')} />}
        {section === 'backup' && <ProfileBackup />}
      </div>
    </dialog>
  )
}
