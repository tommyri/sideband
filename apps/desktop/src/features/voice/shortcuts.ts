const bindings = {
  mute: { key: 'M', alt: false },
  deafen: { key: 'D', alt: true },
  previous: { key: 'ArrowUp', alt: false },
  next: { key: 'ArrowDown', alt: false },
} as const

type Action = keyof typeof bindings

export function shortcutLabel(action: Action) {
  const { key, alt } = bindings[action]
  return ['Ctrl/Cmd', 'Shift', ...(alt ? ['Alt'] : []), key.replace('Arrow', '')].join('+')
}

export function voiceShortcut(event: KeyboardEvent): Action | undefined {
  if (!(event.ctrlKey || event.metaKey) || !event.shiftKey) return
  return (Object.keys(bindings) as Action[]).find((action) => {
    const { key, alt } = bindings[action]
    // Alt+D can produce a different character on international keyboards.
    const matches =
      event.key.toLowerCase() === key.toLowerCase() || (alt && event.code === `Key${key}`)
    return matches && (!alt || event.altKey)
  })
}
