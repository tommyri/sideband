import { Search } from 'lucide-react'

const groups = [
  {
    label: 'Online — 4',
    people: [
      ['Maya Chen', 'Designing', 'MC', 'avatar-1'],
      ['Oskar Lind', 'Reviewing transport', 'OL', 'avatar-2'],
      ['Alex Rivera', 'In Studio', 'AR', 'avatar-3'],
      ['Ada', 'Available', 'A', 'avatar-4'],
    ],
  },
  {
    label: 'Offline — 2',
    people: [
      ['Lin', 'Away', 'L', 'avatar-offline'],
      ['Noah', 'Offline', 'N', 'avatar-offline'],
    ],
  },
]

export function MemberList() {
  return (
    <aside className="member-list">
      <header>
        <strong>Members</strong>
        <Search size={16} />
      </header>
      <div className="member-scroll">
        {groups.map((group) => (
          <section key={group.label}>
            <p className="section-label">{group.label}</p>
            {group.people.map(([name, activity, initials, color]) => (
              <button className="member" key={name}>
                <span className={`member-avatar ${color}`}>{initials}</span>
                <span>
                  <strong>{name}</strong>
                  <small>{activity}</small>
                </span>
              </button>
            ))}
          </section>
        ))}
      </div>
    </aside>
  )
}
