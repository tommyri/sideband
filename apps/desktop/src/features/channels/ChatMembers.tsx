import { Users, X } from 'lucide-react'
import { useLayoutEffect, useRef, useState, type ReactNode } from 'react'

export function ChatMembers({ children }: { children: ReactNode }) {
  const [open, setOpen] = useState(false)
  const trigger = useRef<HTMLButtonElement>(null)
  const close = () => setOpen(false)
  const dialog = useRef<HTMLDialogElement>(null)
  useLayoutEffect(() => {
    const sheet = dialog.current
    if (!sheet || !open) return
    const opener = trigger.current
    sheet.showModal()
    return () => {
      sheet.close()
      if (opener?.isConnected) opener.focus({ preventScroll: true })
    }
  }, [open])
  return (
    <>
      <button
        ref={trigger}
        className="button secondary"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={open ? 'chat-members' : undefined}
        onClick={() => setOpen(true)}
      >
        <Users size={16} /> Members
      </button>
      {open && (
        <dialog
          ref={dialog}
          className="chat-members-sheet"
          id="chat-members"
          aria-labelledby="chat-members-title"
          onCancel={(event) => {
            event.preventDefault()
            close()
          }}
          onClick={(event) => {
            if (event.target === event.currentTarget) {
              const box = event.currentTarget.getBoundingClientRect()
              if (
                event.clientX < box.left ||
                event.clientX > box.right ||
                event.clientY < box.top ||
                event.clientY > box.bottom
              )
                close()
            }
          }}
        >
          <header>
            <h2 id="chat-members-title">Members</h2>
            <button className="icon-button" aria-label="Close members" autoFocus onClick={close}>
              <X size={18} />
            </button>
          </header>
          <div className="chat-members-list">{children}</div>
        </dialog>
      )}
    </>
  )
}
