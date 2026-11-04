import { useEffect, useRef, useState } from 'react'
import type { BridgeAttachment } from '../../lib/generated/BridgeAttachment'
import type { BridgePreview } from '../../lib/generated/BridgePreview'
import { cancelFile, fileSize, mediaBlob, mediaKind, transferFile } from './attachments'
import { errorText, openChatLink } from './api'
import { safeLink } from './rich-model'
function Attachment({
  server,
  file,
  preview,
}: {
  server: string
  file: BridgeAttachment
  preview?: string
}) {
  const [url, setUrl] = useState('')
  const [busy, setBusy] = useState(false)
  const [status, setStatus] = useState('')
  const [lightbox, setLightbox] = useState(false)
  const active = useRef<string | null>(null)
  const blob = useRef('')
  const dialog = useRef<HTMLDialogElement>(null)
  useEffect(
    () => () => {
      if (active.current) void cancelFile(active.current)
      active.current = null
      if (blob.current) URL.revokeObjectURL(blob.current)
    },
    [],
  )
  useEffect(() => {
    if (lightbox) dialog.current?.showModal()
  }, [lightbox])
  const load = async (save: boolean) => {
    const transfer = crypto.randomUUID()
    active.current = transfer
    setBusy(true)
    setStatus('')
    try {
      const result = await transferFile({
        transfer,
        server,
        attachment: file.id,
        filename: file.filename,
        save,
        preview,
      })
      if (active.current !== transfer) return
      if (save) setStatus(`Saved to ${result}`)
      else {
        if (blob.current) URL.revokeObjectURL(blob.current)
        const media: { data: string; mime: string } = preview
          ? JSON.parse(result)
          : { data: result, mime: file.mime }
        blob.current = mediaBlob(media.data, media.mime)
        setUrl(blob.current)
      }
    } catch (e) {
      if (active.current === transfer) setStatus(errorText(e))
    } finally {
      if (active.current === transfer) {
        active.current = null
        setBusy(false)
      }
    }
  }
  const kind = mediaKind(file.mime)
  return (
    <div className="chat-file">
      <div className="chat-file-label">
        <strong>{file.filename}</strong>
        {!preview && <small>{fileSize(file.size)}</small>}
      </div>
      {url && kind === 'image' && (
        <button
          className="chat-image-button"
          aria-label={`Enlarge ${file.filename}`}
          onClick={() => setLightbox(true)}
        >
          <img
            src={url}
            alt={file.filename}
            onError={() => setStatus('This image could not be displayed. Try downloading it.')}
          />
        </button>
      )}
      {url && kind === 'video' && (
        <video controls preload="metadata" src={url} aria-label={file.filename}>
          Video playback is not supported.
        </video>
      )}
      {url && kind === 'audio' && (
        <audio controls preload="metadata" src={url} aria-label={file.filename}>
          Audio playback is not supported.
        </audio>
      )}
      <div className="chat-file-actions">
        {kind && !url && (
          <button disabled={busy} onClick={() => void load(false)}>
            Load {kind}
          </button>
        )}
        {!preview && (
          <button disabled={busy} onClick={() => void load(true)}>
            Download
          </button>
        )}
        {busy && (
          <>
            <progress aria-label={`Downloading ${file.filename}`} />
            <button
              onClick={() => {
                if (active.current) void cancelFile(active.current)
                active.current = null
                setBusy(false)
                setStatus('Transfer cancelled.')
              }}
            >
              Cancel
            </button>
          </>
        )}
      </div>
      {status && <p role="status">{status}</p>}
      {lightbox && (
        <dialog
          ref={dialog}
          className="chat-lightbox"
          onKeyDown={(e) => e.stopPropagation()}
          onClose={() => setLightbox(false)}
          onCancel={() => setLightbox(false)}
        >
          <button autoFocus onClick={() => dialog.current?.close()}>
            Close image
          </button>
          <img src={url} alt={file.filename} />
        </dialog>
      )}
    </div>
  )
}
const hidden = new Set<string>()
function Preview({ preview, identity }: { preview: BridgePreview; identity: string }) {
  const [hide, setHide] = useState(() => hidden.has(identity))
  const [error, setError] = useState('')
  const url = safeLink(preview.url)
  if (!url || !/^https?:/.test(url)) return null
  if (hide)
    return (
      <button
        className="chat-preview-show"
        onClick={() => {
          hidden.delete(identity)
          setHide(false)
        }}
      >
        Show link preview
      </button>
    )
  const open = (value: string) => void openChatLink(value).catch((e) => setError(errorText(e)))
  return (
    <aside className="chat-link-card" aria-label="Link preview">
      <small>{preview.siteName || new URL(url).hostname}</small>
      <button className="chat-inline-link" onClick={() => open(url)}>
        {preview.title || url}
      </button>
      {preview.description && <p>{preview.description}</p>}
      {preview.imageUrl && safeLink(preview.imageUrl).startsWith('https:') && (
        <div>
          <small>External image · loading shares your IP with the image host.</small>
          <Attachment
            key={preview.imageUrl}
            server=""
            preview={preview.imageUrl}
            file={{
              id: '',
              filename: preview.title || 'Preview image',
              mime: 'image/png',
              size: 0,
            }}
          />
        </div>
      )}
      <button
        aria-label="Hide link preview"
        onClick={() => {
          hidden.add(identity)
          setHide(true)
        }}
      >
        Hide preview
      </button>
      {error && <p role="alert">{error}</p>}
    </aside>
  )
}
export function MessageFiles({
  server,
  message,
  attachments,
  previews,
}: {
  server: string
  message: string
  attachments: BridgeAttachment[]
  previews: BridgePreview[]
}) {
  return (
    <>
      <div className="chat-files">
        {attachments.map((file) => (
          <Attachment key={file.id} server={server} file={file} />
        ))}
      </div>
      {previews.map((preview, i) => (
        <Preview
          key={`${preview.url}:${i}`}
          preview={preview}
          identity={JSON.stringify([server, message, preview.url])}
        />
      ))}
    </>
  )
}
