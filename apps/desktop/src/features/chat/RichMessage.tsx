import { Children, memo, useState, type ReactNode } from 'react'
import Markdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import rehypeHighlight from 'rehype-highlight'
import rehypeSanitize, { defaultSchema } from 'rehype-sanitize'
import { canonicalId, emoji, parseMessageLink, safeLink, type ChatMember } from './rich-model'
import { openChatLink, errorText } from './api'
import { navigateMessage } from './navigation'

const schema = {
  ...defaultSchema,
  protocols: { ...defaultSchema.protocols, href: ['http', 'https', 'sideband'] },
  attributes: {
    ...defaultSchema.attributes,
    code: [...(defaultSchema.attributes?.code ?? []), ['className', 'hljs', /^language-/]],
    span: [['className', /^hljs-/]],
  },
}
const plugins = [
  [rehypeHighlight, { detect: false }],
  [rehypeSanitize, schema],
] as NonNullable<React.ComponentProps<typeof Markdown>['rehypePlugins']>
function Spoiler({ children }: { children: ReactNode }) {
  const [open, setOpen] = useState(false)
  return (
    <span className="chat-spoiler">
      <button type="button" aria-expanded={open} onClick={() => setOpen(!open)}>
        {open ? 'Hide spoiler' : 'Reveal spoiler'}
      </button>
      {open && <span>{children}</span>}
    </span>
  )
}
export const RichMessage = memo(function RichMessage({
  content,
  members,
  own,
}: {
  content: string
  members: Map<string, ChatMember>
  own: string | null
}) {
  const [error, setError] = useState('')
  const open = (url: string) => {
    setError('')
    if (parseMessageLink(url)) {
      try {
        navigateMessage(url)
      } catch (e) {
        setError(errorText(e))
      }
    } else void openChatLink(url).catch((e) => setError(errorText(e)))
  }
  const text = (value: string): ReactNode[] => {
    const result: ReactNode[] = []
    let end = 0
    for (const match of value.matchAll(
      /<@([0-9a-z]{26})>|@everyone|:([a-z0-9_+-]+):|sideband:\/\/message\/[^\s<>]+/gi,
    )) {
      const index = match.index
      const raw = match[0]
      result.push(value.slice(end, index))
      end = index + raw.length
      const id = match[1] ? canonicalId(match[1]) : null
      const member = id ? members.get(id) : undefined
      const everyone =
        raw === '@everyone' &&
        !/[a-z0-9]/i.test(value[index - 1] ?? '') &&
        !/[a-z0-9]/i.test(value[end] ?? '')
      if (member || everyone)
        result.push(
          <mark
            key={index}
            className={everyone || id === own ? 'chat-mention own' : 'chat-mention'}
            title={member ? `@${member.username}` : undefined}
          >
            {member ? `@${member.displayName || member.username}` : '@everyone'}
          </mark>,
        )
      else if (match[2] && emoji[match[2]]) result.push(emoji[match[2]])
      else if (parseMessageLink(raw))
        result.push(
          <button key={index} className="chat-inline-link" onClick={() => open(raw)}>
            Open message
          </button>,
        )
      else result.push(raw)
    }
    result.push(value.slice(end))
    return result
  }
  const decorate = (children: ReactNode) => {
    const out: ReactNode[] = []
    let hidden: ReactNode[] | null = null
    for (const child of Children.toArray(children)) {
      if (typeof child !== 'string') {
        ;(hidden ?? out).push(child)
        continue
      }
      const parts = child.split('||')
      for (const [i, part] of parts.entries()) {
        if (i > 0) {
          if (hidden) {
            out.push(<Spoiler key={`spoiler-${out.length}`}>{hidden}</Spoiler>)
            hidden = null
          } else hidden = []
        }
        ;(hidden ?? out).push(...text(part))
      }
    }
    if (hidden) out.push('||', ...hidden)
    return out
  }
  return (
    <div className="chat-rich">
      <Markdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={plugins}
        skipHtml
        allowedElements={[
          'p',
          'strong',
          'em',
          'del',
          'code',
          'pre',
          'blockquote',
          'a',
          'br',
          'ul',
          'ol',
          'li',
          'hr',
          'span',
          'img',
        ]}
        unwrapDisallowed
        urlTransform={safeLink}
        components={{
          p: ({ children }) => <p>{decorate(children)}</p>,
          strong: ({ children }) => <strong>{decorate(children)}</strong>,
          em: ({ children }) => <em>{decorate(children)}</em>,
          del: ({ children }) => <del>{decorate(children)}</del>,
          li: ({ children }) => <li>{decorate(children)}</li>,
          a: ({ children, href }) =>
            href ? (
              <a
                href={href}
                title={href}
                onClick={(e) => {
                  e.preventDefault()
                  open(href)
                }}
              >
                {children}
              </a>
            ) : (
              <span>{children}</span>
            ),
          img: ({ alt }) => <span>[Image: {alt || 'image'}]</span>,
        }}
      >
        {content}
      </Markdown>
      {error && <p role="alert">{error}</p>}
    </div>
  )
})
