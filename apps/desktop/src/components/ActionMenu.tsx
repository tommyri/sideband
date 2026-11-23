import { useId, useLayoutEffect, useRef, useState, type ReactNode, type Ref } from 'react'
import { createPortal } from 'react-dom'

interface MenuAction {
  label: string
  onSelect: () => void
  hidden?: boolean
  disabled?: boolean
  divider?: boolean
}

function enabledItems(menu: HTMLElement) {
  return Array.from(menu.querySelectorAll<HTMLButtonElement>('button:enabled'))
}

export function ActionMenu({
  label,
  className,
  children,
  actions,
  disabled,
  ref,
  returnFocus = (button) => button.focus({ preventScroll: true }),
}: {
  label: string
  className: string
  children: ReactNode
  actions: MenuAction[]
  disabled?: boolean
  ref?: Ref<HTMLButtonElement>
  returnFocus?: (button: HTMLButtonElement) => void
}) {
  const id = useId()
  const [anchor, setAnchor] = useState<HTMLButtonElement | null>(null)
  const openAtEnd = useRef(false)
  const panel = useRef<HTMLDivElement>(null)

  useLayoutEffect(() => {
    const menu = panel.current
    if (!anchor || !menu) return
    const place = () => {
      const rect = anchor.getBoundingClientRect()
      const { offsetWidth: width, offsetHeight: height } = menu
      const below = rect.bottom + 4
      const top = below + height <= window.innerHeight - 8 ? below : rect.top - 4 - height
      menu.style.top = `${Math.max(8, Math.min(top, window.innerHeight - height - 8))}px`
      menu.style.left = `${Math.max(8, Math.min(rect.left, window.innerWidth - width - 8))}px`
    }
    place()
    enabledItems(menu)
      .at(openAtEnd.current ? -1 : 0)
      ?.focus({ preventScroll: true })
    const outside = (event: Event) => {
      if (
        event.target instanceof Node &&
        !anchor.contains(event.target) &&
        !menu.contains(event.target)
      )
        setAnchor(null)
    }
    window.addEventListener('resize', place)
    document.addEventListener('scroll', place, true)
    document.addEventListener('pointerdown', outside, true)
    document.addEventListener('focusin', outside)
    return () => {
      window.removeEventListener('resize', place)
      document.removeEventListener('scroll', place, true)
      document.removeEventListener('pointerdown', outside, true)
      document.removeEventListener('focusin', outside)
    }
  }, [anchor])

  return (
    <>
      <button
        ref={ref}
        type="button"
        disabled={disabled}
        className={className}
        aria-label={label}
        title={label}
        aria-haspopup="menu"
        aria-expanded={!!anchor}
        aria-controls={anchor ? id : undefined}
        onClick={(event) => {
          openAtEnd.current = false
          if (anchor) returnFocus(anchor)
          setAnchor(anchor ? null : event.currentTarget)
        }}
        onKeyDown={(event) => {
          if (event.key === 'Enter' || event.key === ' ') {
            // Let the button activate without also triggering a voice shortcut.
            event.stopPropagation()
            return
          }
          if (event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return
          if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return
          event.preventDefault()
          event.stopPropagation()
          openAtEnd.current = event.key === 'ArrowUp'
          setAnchor(event.currentTarget)
        }}
      >
        {children}
      </button>
      {anchor &&
        createPortal(
          <div
            className="action-menu"
            role="menu"
            aria-label={label}
            ref={panel}
            id={id}
            onKeyDown={(event) => {
              event.stopPropagation()
              if (event.key === 'Tab') {
                // Resume the normal tab order at the trigger, outside the portal.
                anchor.focus({ preventScroll: true })
                setAnchor(null)
                return
              }
              if (event.key === 'Escape') {
                event.preventDefault()
                setAnchor(null)
                returnFocus(anchor)
                return
              }
              const items = enabledItems(event.currentTarget)
              const index = items.indexOf(document.activeElement as HTMLButtonElement)
              let next: number
              switch (event.key) {
                case 'Home':
                  next = 0
                  break
                case 'End':
                  next = items.length - 1
                  break
                case 'ArrowDown':
                  next = (index + 1) % items.length
                  break
                case 'ArrowUp':
                  next = (index + items.length - 1) % items.length
                  break
                default:
                  return
              }
              event.preventDefault()
              items[next]?.focus()
            }}
          >
            {actions
              .filter((action) => !action.hidden)
              .map((action) => (
                <button
                  type="button"
                  role="menuitem"
                  tabIndex={-1}
                  key={action.label}
                  disabled={action.disabled}
                  className={action.divider ? 'menu-divider' : undefined}
                  onClick={() => {
                    setAnchor(null)
                    returnFocus(anchor)
                    action.onSelect()
                  }}
                >
                  {action.label}
                </button>
              ))}
          </div>,
          anchor.closest('.app-shell') ?? document.body,
        )}
    </>
  )
}
