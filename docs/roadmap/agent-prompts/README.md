# Agent-Aufträge

- `NN-*.md`: alle bisher erteilten Aufträge, **wörtlich** und chronologisch.
  - 01–05, 12–15: Recherche (Deep Research) für die Berichte in `reports/`.
  - Ab 06: Arbeitspakete.
  - Der lokale Pfad zum Recherche-Skill ist durch `<deep-research-skill-dir>` ersetzt.
- `next-WPxx-*.md`: fertige Aufträge für die offenen Pakete, in dieser Reihenfolge: WP12 → WP15 → WP14 → WP16.

Wenn ein Agent abbricht, wird er fortgesetzt, nicht neu gestartet: per SendMessage an die Agent-ID oder mit einem neuen Agenten, der auf dem WIP-Commit aufsetzt. Der Nachrichtentext dafür steht in `../HANDOFF.md`, Abschnitt 5.
Ablauf und Regeln: `../HANDOFF.md`.
