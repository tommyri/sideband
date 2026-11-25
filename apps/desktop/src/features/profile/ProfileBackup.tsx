import { useEffect, useId, useRef, useState } from 'react'
import { FileText, KeyRound } from 'lucide-react'
import {
  saveBackup,
  selectBackup,
  restoreBackup,
  type SavedBackup,
  type SelectedBackup,
} from './backup/api'
import './profile-backup.css'

type Props = {
  initialMode?: 'backup' | 'restore'
  onDone?: (name: string) => void
  onCancel?: () => void
}

export function ProfileBackup({ initialMode = 'backup', onDone, onCancel }: Props) {
  const [step, setStep] = useState<'backup' | 'key' | 'saved' | 'restore' | 'restored'>(initialMode)
  const [exported, setExported] = useState<SavedBackup | null>(null)
  const [selected, setSelected] = useState<SelectedBackup | null>(null)
  const [words, setWords] = useState('')
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [copyStatus, setCopyStatus] = useState('')
  const heading = useRef<HTMLHeadingElement>(null)
  const input = useRef<HTMLInputElement>(null)
  const id = useId()
  const active = useRef(true)
  useEffect(() => {
    active.current = true
    return () => {
      active.current = false
    }
  }, [])
  useEffect(() => {
    if (step !== initialMode) heading.current?.focus()
  }, [step, initialMode])

  async function run(action: () => Promise<void>) {
    if (busy) return
    setBusy(true)
    setError('')
    try {
      await action()
    } catch (failure) {
      if (active.current) {
        setError(
          (failure as { message?: string })?.message ??
            'Sideband couldn’t finish. Please try again.',
        )
      }
    } finally {
      if (active.current) setBusy(false)
    }
  }

  function save() {
    void run(async () => {
      const backup = await saveBackup()
      if (!backup || !active.current) return
      setExported(backup)
      setCopyStatus('')
      setStep('key')
    })
  }

  function choose() {
    void run(async () => {
      const file = await selectBackup()
      if (!file || !active.current) return
      setSelected(file)
      setWords('')
    })
  }
  useEffect(() => {
    if (selected) input.current?.focus()
  }, [selected, busy])

  function restore() {
    if (!selected || !words.trim()) return
    void run(async () => {
      const restoredName = await restoreBackup(selected.id, words)
      if (!active.current) return
      setWords('')
      setSelected(null)
      setName(restoredName)
      setStep('restored')
    })
  }

  const title = {
    backup: 'Back up your profile',
    key: 'Keep your recovery words',
    saved: 'Your backup is ready',
    restore: 'Recover your profile',
    restored: 'Profile restored',
  }[step]
  return (
    <section className="profile-backup" aria-labelledby={id} aria-busy={busy}>
      <h3 ref={heading} id={id} tabIndex={-1}>
        {title}
      </h3>
      {step === 'backup' && (
        <>
          <p>If you lose your devices, you’ll need two things to get your profile back.</p>
          <div className="backup-instruction">
            <FileText aria-hidden="true" />
            <div>
              <strong>1. Save the backup file</strong>
              <p>
                Save this encrypted file in a private folder in iCloud Drive or Google Drive. Choose
                an account you can open even if you lose this computer.
              </p>
            </div>
          </div>
          <div className="backup-instruction">
            <KeyRound aria-hidden="true" />
            <div>
              <strong>2. Keep the recovery key</strong>
              <p>
                These words unlock the file. Keep them in your password manager or on paper in a
                safe place.
              </p>
            </div>
          </div>
          <button type="button" className="button primary" disabled={busy} onClick={save}>
            Save backup file
          </button>
          <details>
            <summary>Other places to keep the file</summary>
            <p>
              A USB drive works too. If your password manager lets you attach files, you can keep
              the file there and store the words on paper.
            </p>
          </details>
        </>
      )}
      {step === 'key' && exported && (
        <>
          <p>
            You saved <strong>{exported.filename}</strong>. These 12 words open that file. Keep them
            in order.
          </p>
          <p className="backup-words" aria-label="Recovery words">
            {exported.words}
          </p>
          <p>
            Save the words in your password manager or write them on paper. If the file is in your
            password manager, keep the words on paper.
          </p>
          <div className="backup-actions">
            <button
              type="button"
              className="button secondary"
              onClick={async () => {
                try {
                  await navigator.clipboard.writeText(exported.words)
                  setCopyStatus(
                    'Words copied. Save them somewhere you can reach without this computer.',
                  )
                } catch {
                  setCopyStatus('Select the words above to copy them, or write them on paper.')
                }
              }}
            >
              Copy words
            </button>
            <button
              type="button"
              className="button primary"
              onClick={() => {
                setExported(null)
                setStep('saved')
              }}
            >
              I saved my key
            </button>
          </div>
          {copyStatus && <p role="status">{copyStatus}</p>}
        </>
      )}
      {step === 'saved' && (
        <>
          <p>
            On a new computer, choose <strong>Restore from backup</strong>, open the backup file,
            and enter the words you saved with it.
          </p>
          <button type="button" className="button secondary" onClick={() => setStep('backup')}>
            Save another backup
          </button>
        </>
      )}
      {step === 'restore' && (
        <>
          <p>
            Choose your backup file. Then type or paste the words you kept when you saved it. Those
            words are your recovery key.
          </p>
          <div>
            <strong>Backup file</strong>
            <div className="backup-actions">
              {selected && <span className="backup-filename">{selected.filename}</span>}
              <button type="button" className="button secondary" disabled={busy} onClick={choose}>
                {selected ? 'Change file' : 'Choose backup file'}
              </button>
            </div>
          </div>
          <label>
            Recovery key
            <input
              type="password"
              ref={input}
              value={words}
              onChange={(e) => {
                setWords(e.target.value)
                setError('')
              }}
              autoComplete="off"
              autoCapitalize="none"
              spellCheck={false}
              disabled={busy}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault()
                  restore()
                }
              }}
            />
          </label>
          <div className="backup-actions">
            <button
              type="button"
              className="button primary"
              disabled={busy || !selected || !words.trim()}
              onClick={restore}
            >
              Restore profile
            </button>
            {onCancel && (
              <button type="button" className="button secondary" disabled={busy} onClick={onCancel}>
                Back
              </button>
            )}
          </div>
          <details>
            <summary>Missing your file or key?</summary>
            <p>
              <strong>Missing the file?</strong> Check your private cloud folder, USB drive and
              password manager attachments. You can use another copy of the same file.
            </p>
            <p>
              <strong>Missing the words?</strong> Look in your password manager or where you keep
              important papers. If you still have a trusted computer, save a new backup and its
              words there.
            </p>
            <p>
              If you’ve lost both access to your devices and one of these two items, Sideband can’t
              open this backup. Your server host can help with options for that community.
            </p>
          </details>
        </>
      )}
      {step === 'restored' && (
        <p>
          <strong>{name}</strong> is ready on this computer. Reconnect to your servers to confirm
          access.
        </p>
      )}
      {step === 'restored' && onDone && (
        <button type="button" className="button primary" onClick={() => onDone(name)}>
          Continue
        </button>
      )}
      {error && (
        <p role="alert" className="backup-error">
          {error}
        </p>
      )}
    </section>
  )
}
