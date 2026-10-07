//! Interactive tutorial content: legacy bv's structured tutorial pages
//! (`pkg/ui/tutorial_content.go`), with bvr's key bindings. The TUI renders
//! them in the `` ` `` tutorial modal.

/// Accent colors a tutorial element can carry (status-flow boxes, info
/// boxes), mapped to theme tokens by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    Open,
    InProgress,
    Blocked,
    Closed,
    Primary,
    Feature,
}

/// A node of a [`Element::Tree`] diagram.
#[derive(Debug)]
pub struct TreeNode {
    pub label: &'static str,
    pub children: &'static [TreeNode],
}

/// One typed building block of a tutorial page.
#[derive(Debug)]
pub enum Element {
    /// Bold primary heading with a muted underline.
    Section(&'static str),
    /// Wrapped body text.
    Paragraph(&'static str),
    /// Blank lines.
    Spacer(u8),
    /// `•` bulleted list.
    Bullet(&'static [&'static str]),
    /// Rounded `💡 TIP` box.
    Tip(&'static str),
    /// Rounded `ℹ️ NOTE` box.
    Note(&'static str),
    /// Rounded `⚠️ WARN` box.
    Warning(&'static str),
    /// Numbered/iconic value proposition: icon, text.
    ValueProp(&'static str, &'static str),
    /// Code block with a left accent bar (may span lines).
    Code(&'static str),
    /// Key / description rows.
    KeyTable(&'static [(&'static str, &'static str)]),
    /// Boxed steps joined by arrows.
    StatusFlow(&'static [(&'static str, Accent)]),
    /// Root label and children drawn with tree connectors.
    Tree(&'static str, &'static [TreeNode]),
    /// Rounded box with a colored title and body.
    InfoBox(&'static str, &'static str, Accent),
    /// Horizontal rule.
    Divider,
}

/// One tutorial page.
#[derive(Debug)]
pub struct Page {
    /// Stable identifier (Go's page IDs, e.g. `intro-welcome`).
    pub id: &'static str,
    pub title: &'static str,
    /// Table-of-contents section (Introduction, Core Concepts, ...).
    pub section: &'static str,
    pub elements: &'static [Element],
}

/// Every tutorial page in reading order.
#[must_use]
pub fn pages() -> &'static [Page] {
    PAGES
}

static PAGES: &[Page] = &[
    // =============================================================
    // INTRODUCTION (4 pages)
    // =============================================================
    Page {
        id: "intro-welcome",
        title: "Welcome",
        section: "Introduction",
        elements: &[
            Element::Section("Welcome to beads_viewer"),
            Element::Paragraph("Issue tracking that lives in your code."),
            Element::Spacer(1),
            Element::Paragraph(
                "The problem: You're deep in flow, coding away, when you need to check an issue. You switch to a browser, navigate to your tracker, lose context, and break concentration.",
            ),
            Element::Spacer(1),
            Element::Paragraph(
                "The solution: bvr brings issue tracking into your terminal, where you already work. No browser tabs. No context switching. No cloud dependencies.",
            ),
            Element::Spacer(1),
            Element::Section("The 30-Second Value Proposition"),
            Element::Bullet(&[
                "Issues live in your repo - version controlled, diffable, greppable",
                "Works offline - no internet required, no accounts to manage",
                "AI-native - designed for both humans and coding agents",
                "Zero dependencies - just a single binary and your git repo",
            ]),
            Element::Spacer(1),
            Element::Tip("Press -> or Space to continue"),
        ],
    },
    Page {
        id: "intro-philosophy",
        title: "The Beads Philosophy",
        section: "Introduction",
        elements: &[
            Element::Section("Why \"beads\"?"),
            Element::Paragraph(
                "Think of git commits as beads on a string - each one a discrete, meaningful step in your project's history. Issues are beads too.",
            ),
            Element::Spacer(1),
            Element::Section("Core Principles"),
            Element::Spacer(1),
            Element::ValueProp(
                "①",
                "Issues as First-Class Citizens - Your .beads/ directory gets the same git treatment as code: branching, merging, history.",
            ),
            Element::Spacer(1),
            Element::ValueProp(
                "②",
                "No External Dependencies - No servers. No accounts. No API keys. Git + terminal = everything.",
            ),
            Element::Spacer(1),
            Element::ValueProp(
                "③",
                "Diffable and Greppable - Issues stored as plain JSONL. Git diff your backlog. Grep for patterns.",
            ),
            Element::Spacer(1),
            Element::ValueProp(
                "④",
                "Human and Agent Readable - Same data works for humans (bvr) and AI agents (--robot-* flags).",
            ),
        ],
    },
    Page {
        id: "intro-audience",
        title: "Who Is This For?",
        section: "Introduction",
        elements: &[
            Element::Section("Solo Developers"),
            Element::Paragraph(
                "Managing personal projects? Keep your TODO lists organized without heavyweight tools. Everything stays in your repo, backs up with your code.",
            ),
            Element::Spacer(1),
            Element::Section("Small Teams"),
            Element::Paragraph(
                "Want lightweight issue tracking without subscription fees? Share your .beads/ directory through git. Everyone sees the same state.",
            ),
            Element::Spacer(1),
            Element::Section("AI Coding Agents"),
            Element::Paragraph(
                "This is where bvr shines. AI agents need structured task management. The --robot-* flags output machine-readable JSON:",
            ),
            Element::Spacer(1),
            Element::Code(
                "bvr --robot-triage    # What should I work on?\nbvr --robot-plan      # How can work be parallelized?",
            ),
            Element::Spacer(1),
            Element::Section("Anyone Tired of Context-Switching"),
            Element::Paragraph(
                "If you've ever lost your train of thought switching between your editor and a web-based tracker, bvr is for you.",
            ),
        ],
    },
    Page {
        id: "intro-quickstart",
        title: "Quick Start",
        section: "Introduction",
        elements: &[
            Element::Section("You're already running bvr!"),
            Element::Spacer(1),
            Element::Section("Basic Navigation"),
            Element::KeyTable(&[
                ("j / k", "Move down / up"),
                ("Enter", "Open issue details"),
                ("Esc", "Close overlay / go back"),
                ("q", "Quit bvr"),
            ]),
            Element::Spacer(1),
            Element::Section("Switching Views"),
            Element::KeyTable(&[
                ("b", "Board (Kanban)"),
                ("g", "Graph (dependencies)"),
                ("i", "Insights panel"),
                ("H", "History"),
            ]),
            Element::Spacer(1),
            Element::Section("Getting Help"),
            Element::KeyTable(&[
                ("?", "Quick help overlay"),
                ("`", "This tutorial"),
                ("; / F2", "Shortcuts sidebar"),
            ]),
            Element::Spacer(1),
            Element::Tip("Press t to see Table of Contents"),
        ],
    },
    // =============================================================
    // CORE CONCEPTS (5 pages)
    // =============================================================
    Page {
        id: "concepts-beads",
        title: "What Are Beads?",
        section: "Core Concepts",
        elements: &[
            Element::Section("A bead is a unit of work"),
            Element::Paragraph(
                "Think of your project's work as beads on a string - discrete items that together form the complete picture.",
            ),
            Element::Spacer(1),
            Element::Section("Issue Types"),
            Element::KeyTable(&[
                ("bug", "Something broken that needs fixing"),
                ("feature", "New functionality to add"),
                ("task", "General work item"),
                ("epic", "Large initiative with sub-tasks"),
                ("chore", "Maintenance, cleanup, tech debt"),
            ]),
            Element::Spacer(1),
            Element::Section("Storage"),
            Element::Paragraph(
                "Issues live in .beads/beads.jsonl (or .beads/issues.jsonl) - a simple JSON Lines file:",
            ),
            Element::Bullet(&[
                "Version controlled - branch, merge, history",
                "Diffable - see exactly what changed",
                "Greppable - search with standard tools",
            ]),
        ],
    },
    Page {
        id: "concepts-dependencies",
        title: "Dependencies & Blocking",
        section: "Core Concepts",
        elements: &[
            Element::Section("Not all work can happen in parallel"),
            Element::Paragraph(
                "Some issues must wait for others. This is where dependencies come in.",
            ),
            Element::Spacer(1),
            Element::Section("The Relationship"),
            Element::StatusFlow(&[("Auth Fix", Accent::Open), ("Deploy", Accent::Blocked)]),
            Element::Spacer(1),
            Element::Paragraph("Auth Fix BLOCKS Deploy. You can't deploy until auth is fixed."),
            Element::Spacer(1),
            Element::Section("Visual Indicators"),
            Element::KeyTable(&[
                ("Red", "Blocked - waiting on something"),
                ("Green", "Ready - no blockers, can start"),
                ("->", "Shows what this issue blocks"),
                ("<-", "Shows what blocks this issue"),
            ]),
            Element::Spacer(1),
            Element::Section("The Ready Filter"),
            Element::Paragraph("Press r to filter to ready issues: Open + Zero Blockers"),
            Element::Spacer(1),
            Element::Tip("Start your day with 'br ready' to see actionable work"),
        ],
    },
    Page {
        id: "concepts-labels",
        title: "Labels & Organization",
        section: "Core Concepts",
        elements: &[
            Element::Section("Flexible categorization"),
            Element::Paragraph(
                "Labels provide flexible categorization that cuts across types and priorities.",
            ),
            Element::Spacer(1),
            Element::Section("Common Label Patterns"),
            Element::KeyTable(&[
                ("Area", "frontend, backend, api, database"),
                ("Owner", "team-alpha, @alice, contractor"),
                ("Scope", "mvp, v2, tech-debt, nice-to-have"),
                ("State", "needs-review, blocked-external"),
            ]),
            Element::Spacer(1),
            Element::Section("Working with Labels"),
            Element::KeyTable(&[
                ("L", "Open label picker (filter by label)"),
                ("[", "Label health dashboard"),
                ("]", "Label attention view"),
            ]),
            Element::Spacer(1),
            Element::Tip("Keep your label set small. Too many = no one uses them."),
        ],
    },
    Page {
        id: "concepts-priorities",
        title: "Priorities & Status",
        section: "Core Concepts",
        elements: &[
            Element::Section("How important? Where in the workflow?"),
            Element::Spacer(1),
            Element::Section("Priority Levels"),
            Element::KeyTable(&[
                ("P0", "Critical/emergency - drop everything"),
                ("P1", "High priority - this sprint/week"),
                ("P2", "Medium - this cycle/month"),
                ("P3", "Low - when you have time"),
                ("P4", "Backlog - someday/maybe"),
            ]),
            Element::Spacer(1),
            Element::Section("Status Flow"),
            Element::StatusFlow(&[
                ("open", Accent::Open),
                ("in_progress", Accent::InProgress),
                ("closed", Accent::Closed),
            ]),
            Element::Spacer(1),
            Element::Section("Changing Priority/Status"),
            Element::Paragraph(
                "bvr is a read-only viewer; change issues with br, and bvr picks up the new state:",
            ),
            Element::Code("br update ID --priority=1\nbr update ID --status=in_progress"),
            Element::KeyTable(&[("p", "Priority hints (suggested re-prioritizations)")]),
            Element::Spacer(1),
            Element::Tip("If everything is P0, nothing is P0"),
        ],
    },
    Page {
        id: "concepts-graph",
        title: "The Dependency Graph",
        section: "Core Concepts",
        elements: &[
            Element::Section("Your issues form a directed graph"),
            Element::Paragraph("Work flows in one direction - no cycles allowed."),
            Element::Spacer(1),
            Element::Section("Example Dependency Tree"),
            Element::Tree(
                "Epic: User Auth (bv-001)",
                &[
                    TreeNode {
                        label: "Login Form (bv-002)",
                        children: &[TreeNode {
                            label: "Login Tests (bv-005)",
                            children: &[],
                        }],
                    },
                    TreeNode {
                        label: "Signup Form (bv-003)",
                        children: &[TreeNode {
                            label: "Signup Tests (bv-006)",
                            children: &[],
                        }],
                    },
                    TreeNode {
                        label: "Password Reset (bv-004)",
                        children: &[],
                    },
                ],
            ),
            Element::Spacer(1),
            Element::Section("Key Insights"),
            Element::Bullet(&[
                "Root nodes (no arrows in) → Can start immediately",
                "Leaf nodes (no arrows out) → Nothing depends on them",
                "High fan-out → Completing this unblocks many items",
                "Critical path → Longest chain = minimum time",
            ]),
            Element::Spacer(1),
            Element::Section("Visual Encoding"),
            Element::KeyTable(&[
                ("Node size", "Priority (bigger = higher)"),
                ("Green", "Closed"),
                ("Blue", "In progress"),
                ("Red", "Blocked"),
                ("A → B", "A blocks B"),
            ]),
            Element::Spacer(1),
            Element::Section("What to Look For"),
            Element::Bullet(&[
                "Bottlenecks: One issue blocking many others",
                "Parallel tracks: Independent work streams",
                "Priority inversions: Low-priority blocking high-priority",
            ]),
        ],
    },
    // =============================================================
    // VIEWS & NAVIGATION (8 pages)
    // =============================================================
    Page {
        id: "views-nav-fundamentals",
        title: "Navigation Fundamentals",
        section: "Views",
        elements: &[
            Element::Section("Vim-style navigation throughout"),
            Element::Paragraph(
                "If you know vim, you're already at home. If not, you'll pick it up in minutes.",
            ),
            Element::Spacer(1),
            Element::Section("Core Movement"),
            Element::KeyTable(&[
                ("j", "Move down"),
                ("k", "Move up"),
                ("h", "Move left (multi-column)"),
                ("l", "Move right (multi-column)"),
            ]),
            Element::Spacer(1),
            Element::Section("Jump Commands"),
            Element::KeyTable(&[
                ("gg", "Jump to top"),
                ("G", "Jump to bottom"),
                ("Ctrl+d", "Half-page down"),
                ("Ctrl+u", "Half-page up"),
            ]),
            Element::Spacer(1),
            Element::Section("Universal Keys"),
            Element::KeyTable(&[
                ("?", "Help overlay"),
                ("Esc", "Close / go back"),
                ("Enter", "Select / open"),
                ("q", "Quit bvr"),
            ]),
            Element::Spacer(1),
            Element::Tip("Press ; (or F2) for a shortcuts sidebar that stays visible"),
        ],
    },
    Page {
        id: "views-list",
        title: "List View",
        section: "Views",
        elements: &[
            Element::Section("Your issue inbox"),
            Element::Paragraph("This is where you'll spend most of your time."),
            Element::Spacer(1),
            Element::Section("Filtering"),
            Element::KeyTable(&[
                ("o", "Open issues only"),
                ("c", "Closed issues only"),
                ("r", "Ready (no blockers)"),
                ("I / B", "In progress / blocked"),
                ("a", "All (reset filter)"),
            ]),
            Element::Spacer(1),
            Element::Section("Searching"),
            Element::KeyTable(&[
                ("/", "Search (literal text match)"),
                ("n / N", "Next / prev result"),
            ]),
            Element::Paragraph(
                "Semantic and hybrid ranking live in the CLI: bvr --search \"query\" --robot-search --search-mode hybrid",
            ),
            Element::Spacer(1),
            Element::Section("Sorting"),
            Element::Paragraph(
                "Press s to cycle: default -> created -> priority -> updated -> PageRank -> blockers.",
            ),
            Element::Spacer(1),
            Element::Tip("Filter to r (ready) and work top-down for daily triage"),
        ],
    },
    Page {
        id: "views-detail",
        title: "Detail View",
        section: "Views",
        elements: &[
            Element::Section("Full issue details"),
            Element::Paragraph("Press Enter on any issue to see its full details."),
            Element::Spacer(1),
            Element::Section("What You See"),
            Element::Bullet(&[
                "Status, Priority, Type, Created date",
                "Full description with markdown rendering",
                "Dependencies (what it blocks, what blocks it)",
                "Labels and other metadata",
            ]),
            Element::Spacer(1),
            Element::Section("Detail View Actions"),
            Element::KeyTable(&[
                ("O", "Open in $EDITOR"),
                ("C", "Copy issue ID to clipboard"),
                ("x", "Export issue to markdown"),
                ("j / k", "Scroll content"),
                ("Esc", "Return to list"),
            ]),
            Element::Spacer(1),
            Element::Section("Markdown Support"),
            Element::Paragraph(
                "Descriptions render with headers, bold, code blocks, lists, and tables.",
            ),
        ],
    },
    Page {
        id: "views-split",
        title: "Split View",
        section: "Views",
        elements: &[
            Element::Section("List and detail side by side"),
            Element::Paragraph(
                "On a wide terminal the list and detail panes sit side by side. Press Tab to move focus between them.",
            ),
            Element::Spacer(1),
            Element::Section("Navigation"),
            Element::KeyTable(&[
                ("Tab", "Switch focus between panes"),
                ("j / k", "Navigate in focused pane"),
                ("Esc", "Return to full list"),
            ]),
            Element::Spacer(1),
            Element::Section("When to Use"),
            Element::Bullet(&[
                "Code review: Quickly scan multiple issues",
                "Triage session: Read details without losing context",
                "Dependency analysis: Navigate while viewing relationships",
            ]),
            Element::Spacer(1),
            Element::Tip("Detail pane auto-updates as you navigate the list"),
        ],
    },
    Page {
        id: "views-board",
        title: "Board View",
        section: "Views",
        elements: &[
            Element::Section("Kanban-style board"),
            Element::Paragraph("Press b to switch to the board view."),
            Element::Spacer(1),
            Element::Section("Navigation"),
            Element::KeyTable(&[
                ("h / l", "Move between columns"),
                ("j / k", "Move within column"),
                ("1-4", "Jump to lane"),
                ("Tab", "Toggle detail panel"),
                ("Enter", "View issue details"),
            ]),
            Element::Spacer(1),
            Element::Section("Grouping Modes"),
            Element::KeyTable(&[
                ("s", "Cycle: Status -> Priority -> Type"),
                ("e", "Toggle empty columns"),
            ]),
            Element::Spacer(1),
            Element::Section("Card Border Colors"),
            Element::KeyTable(&[
                ("Red", "Has blockers"),
                ("Yellow", "High-impact (blocks others)"),
                ("Green", "Ready to work"),
            ]),
        ],
    },
    Page {
        id: "views-graph",
        title: "Graph View",
        section: "Views",
        elements: &[
            Element::Section("Visualize dependencies"),
            Element::Paragraph("Press g to see issues as a dependency graph."),
            Element::Spacer(1),
            Element::Section("Reading the Graph"),
            Element::Bullet(&[
                "Arrows point TO what's blocked (A->B = A blocks B)",
                "Node size reflects priority",
                "Color indicates status",
                "Highlighted node is your selection",
            ]),
            Element::Spacer(1),
            Element::Section("Navigation"),
            Element::KeyTable(&[
                ("j / k", "Navigate between nodes"),
                ("h / l", "Navigate siblings"),
                ("Tab", "Cycle through edges"),
                ("/", "Search nodes"),
                ("Enter", "View selected issue"),
            ]),
            Element::Spacer(1),
            Element::Section("Use Cases"),
            Element::Bullet(&[
                "Critical path analysis",
                "Dependency planning",
                "Impact assessment",
            ]),
        ],
    },
    Page {
        id: "views-insights",
        title: "Insights Panel",
        section: "Views",
        elements: &[
            Element::Section("AI-powered prioritization"),
            Element::Paragraph("Press i to open the Insights panel."),
            Element::Spacer(1),
            Element::Section("Priority Score Factors"),
            Element::Bullet(&[
                "Explicit priority (P0-P4)",
                "Blocking factor - how many issues it unblocks",
                "Freshness - recently updated scores higher",
                "Type weight - bugs often over features",
            ]),
            Element::Spacer(1),
            Element::Section("Attention Scores"),
            Element::Paragraph("The panel highlights issues needing attention:"),
            Element::Bullet(&[
                "Stale issues: Open too long without updates",
                "Blocked chains: Issues creating bottlenecks",
                "Priority inversions: Low blocking high",
            ]),
            Element::Spacer(1),
            Element::Section("Panel Keys"),
            Element::KeyTable(&[
                ("h / l", "Switch metric panels"),
                ("e", "Toggle explanations"),
                ("x", "Show calculation proof"),
            ]),
            Element::Spacer(1),
            Element::Section("Heatmap Mode"),
            Element::Paragraph(
                "Press m to color by attention: Red=high, Yellow=moderate, Green=on track",
            ),
        ],
    },
    Page {
        id: "views-history",
        title: "History View",
        section: "Views",
        elements: &[
            Element::Section("Git-integrated timeline"),
            Element::Paragraph("Press H to see commits correlated with bead changes."),
            Element::Spacer(1),
            Element::Section("Navigation"),
            Element::KeyTable(&[
                ("j / k", "Navigate timeline"),
                ("J / K", "Navigate details"),
                ("v", "Toggle Bead/Git mode"),
                ("f", "Toggle file tree panel"),
                ("c", "Cycle confidence filter"),
                ("y", "Copy commit SHA"),
                ("o", "Open commit in browser"),
                ("/", "Search"),
            ]),
            Element::Spacer(1),
            Element::Section("Causality Markers"),
            Element::KeyTable(&[
                ("Direct", "Commit mentions bead ID"),
                ("Temporal", "Within time window"),
                ("File", "Touches associated files"),
            ]),
            Element::Spacer(1),
            Element::Section("Time Travel"),
            Element::Paragraph(
                "Press t and enter a git ref to diff the project against that point (read-only).",
            ),
            Element::Spacer(1),
            Element::Tip("Use t for time travel with git ref input"),
        ],
    },
    // =============================================================
    // ADVANCED FEATURES (7 pages)
    // =============================================================
    Page {
        id: "advanced-semantic-search",
        title: "Semantic + Hybrid Search",
        section: "Advanced",
        elements: &[
            Element::Section("Find matching issues, then rank by importance"),
            Element::Paragraph(
                "bvr --search scores every issue's text against your query (ID and title matches weigh most) and returns ranked JSON with --robot-search.",
            ),
            Element::Paragraph(
                "Hybrid mode re-ranks those text matches using graph signals (PageRank, impact, status, priority, recency), so results stay relevant while surfacing what matters most.",
            ),
            Element::Spacer(1),
            Element::Section("Search Modes"),
            Element::KeyTable(&[
                ("/", "TUI search (literal text)"),
                ("--search-mode text", "CLI text relevance ranking"),
                ("--search-mode hybrid", "CLI ranking (text + graph)"),
                ("--search-preset", "Pick a hybrid weighting preset"),
            ]),
            Element::Spacer(1),
            Element::Section("Example"),
            Element::Paragraph("Searching \"permissions\":"),
            Element::Bullet(&[
                "/ in the TUI jumps between issues containing the word permissions",
                "Text mode ranks them by how strongly they match",
                "Hybrid keeps those results but floats the ones with higher impact",
            ]),
            Element::Spacer(1),
            Element::Section("Tuning"),
            Element::Code(
                "bvr --search \"permissions\" --robot-search --search-mode hybrid\nbvr --search \"permissions\" --robot-search --search-mode hybrid --search-preset impact-first\nbvr --search \"permissions\" --robot-search --search-mode hybrid \\\n  --search-weights '{\"text\":0.4,\"pagerank\":0.2,\"status\":0.15,\"impact\":0.1,\"priority\":0.1,\"recency\":0.05}'",
            ),
            Element::Spacer(1),
            Element::Tip(
                "Start with text mode and switch to hybrid when you want the most important matches surfaced.",
            ),
        ],
    },
    Page {
        id: "advanced-time-travel",
        title: "Time Travel",
        section: "Advanced",
        elements: &[
            Element::Section("See how your project looked at any point"),
            Element::Spacer(1),
            Element::Section("Accessing Time Travel"),
            Element::KeyTable(&[
                ("t", "Full time travel with git ref input"),
                ("H", "History view (visual timeline)"),
            ]),
            Element::Spacer(1),
            Element::Section("Git Reference Syntax"),
            Element::KeyTable(&[
                ("HEAD~5", "5 commits ago"),
                ("main", "Tip of main branch"),
                ("v1.2.0", "Tagged release"),
                ("@{2.weeks.ago}", "Two weeks back"),
            ]),
            Element::Spacer(1),
            Element::Section("Use Cases"),
            Element::Bullet(&[
                "Sprint review: What did we accomplish?",
                "Debugging: When did this get blocked?",
                "Onboarding: What was the project like 6mo ago?",
            ]),
        ],
    },
    Page {
        id: "advanced-label-analytics",
        title: "Label Analytics",
        section: "Advanced",
        elements: &[
            Element::Section("Labels are a lens for understanding"),
            Element::Paragraph("Press [ to open the Labels dashboard."),
            Element::Spacer(1),
            Element::Section("Health Indicators"),
            Element::KeyTable(&[
                ("OK", "Healthy - good progress, few blockers"),
                ("WARN", "Warning - stale or slow velocity"),
                ("CRIT", "Critical - high blocked ratio"),
            ]),
            Element::Spacer(1),
            Element::Section("Health Score Factors"),
            Element::Bullet(&[
                "Velocity: How fast are issues closing?",
                "Staleness: Are old issues piling up?",
                "Blocked ratio: What % is stuck?",
                "Work distribution: Is work spread evenly?",
            ]),
            Element::Spacer(1),
            Element::Section("Cross-Label Flow"),
            Element::Paragraph("Press f to open the flow matrix and see which areas block others."),
        ],
    },
    Page {
        id: "advanced-export",
        title: "Export & Deployment",
        section: "Advanced",
        elements: &[
            Element::Section("Share with non-terminal users"),
            Element::Spacer(1),
            Element::Section("Quick Markdown Export"),
            Element::Paragraph(
                "Press x in the list view to export the selected issue to markdown, or run bvr --export-md report.md for a full report. Great for Slack, email, meeting notes.",
            ),
            Element::Spacer(1),
            Element::Section("Static Site Generation"),
            Element::Code(
                "bvr --pages              # Interactive wizard\nbvr --export-pages ./out # Export to directory\nbvr --preview-pages ./out # Preview locally",
            ),
            Element::Spacer(1),
            Element::Section("Output Includes"),
            Element::Bullet(&[
                "Triage recommendations",
                "Dependency graph visualization",
                "Full-text search",
                "Works offline - no server required",
            ]),
            Element::Spacer(1),
            Element::Tip("Deploy to GitHub Pages or Cloudflare Pages"),
        ],
    },
    Page {
        id: "advanced-workspace",
        title: "Workspace Mode",
        section: "Advanced",
        elements: &[
            Element::Section("Multiple repos, unified view"),
            Element::Spacer(1),
            Element::Section("When to Use"),
            Element::Bullet(&[
                "Monorepo alternatives: Multiple related repos",
                "Microservices: Track issues across services",
                "Frontend + Backend: Separate repos, unified view",
            ]),
            Element::Spacer(1),
            Element::Section("Setup"),
            Element::Paragraph(
                "Create .bv/workspace.yaml with repo paths and prefixes, or pass --workspace <path>.",
            ),
            Element::Spacer(1),
            Element::Section("Navigation"),
            Element::KeyTable(&[("w", "Toggle repo picker")]),
            Element::Spacer(1),
            Element::Section("Cross-Repo Dependencies"),
            Element::Paragraph(
                "Issues can depend on issues in other repos. The graph shows these relationships.",
            ),
        ],
    },
    Page {
        id: "advanced-recipes",
        title: "Recipes",
        section: "Advanced",
        elements: &[
            Element::Section("Saved filter combinations"),
            Element::Paragraph("Press ' (single quote) to open the recipe picker."),
            Element::Spacer(1),
            Element::Section("Built-in Recipes"),
            Element::KeyTable(&[
                ("actionable", "Ready to work (no open blockers)"),
                ("quick-wins", "Easy low-priority actionable items"),
                ("blocked", "Stuck items waiting on dependencies"),
                ("high-impact", "Highest graph centrality"),
                ("triage", "Sorted by computed triage score"),
                ("stale", "No updates in 30+ days"),
            ]),
            Element::Spacer(1),
            Element::Section("From the CLI"),
            Element::Code("bvr --recipe actionable --robot-plan"),
            Element::Spacer(1),
            Element::Section("Filter Options"),
            Element::Paragraph(
                "status, labels, priority min/max, actionable, has blockers, title contains",
            ),
        ],
    },
    Page {
        id: "advanced-ai",
        title: "AI Agent Integration",
        section: "Advanced",
        elements: &[
            Element::Section("Designed for AI coding agents"),
            Element::Spacer(1),
            Element::Section("Human vs Agent"),
            Element::KeyTable(&[
                ("bvr", "Interactive TUI for humans"),
                ("--robot-*", "JSON output for agents"),
            ]),
            Element::Spacer(1),
            Element::Section("Key Robot Commands"),
            Element::Code(
                "bvr --robot-triage  # The mega-command\nbvr --robot-next    # Single top priority\nbvr --robot-plan    # Parallel execution\nbvr --robot-alerts  # Stale, inversions",
            ),
            Element::Spacer(1),
            Element::Section("Agent Workflow"),
            Element::Bullet(&[
                "Call: bvr --robot-next",
                "Claim: br update ID --status=in_progress",
                "Work: Do the implementation",
                "Complete: br close ID",
                "Repeat",
            ]),
            Element::Spacer(1),
            Element::Tip("Every project should have AGENTS.md explaining robot commands"),
        ],
    },
    // =============================================================
    // WORKFLOWS (5 pages)
    // =============================================================
    Page {
        id: "workflow-new-feature",
        title: "Starting a New Feature",
        section: "Workflows",
        elements: &[
            Element::Section("Feature implementation walkthrough"),
            Element::Spacer(1),
            Element::Section("Step 1: Find Available Work"),
            Element::Code("br ready  # Show actionable issues"),
            Element::Paragraph("Or in bvr: press r to filter to ready issues."),
            Element::Spacer(1),
            Element::Section("Step 2: Review & Claim"),
            Element::Bullet(&[
                "Enter: View full details",
                "g: See dependency graph",
                "br update ID --status=in_progress",
            ]),
            Element::Spacer(1),
            Element::Section("Step 3: Create Sub-Tasks"),
            Element::Code(
                "br create --title=\"Implement logic\" --type=task\nbr dep add bv-tests bv-endpoint",
            ),
            Element::Spacer(1),
            Element::Section("Step 4: Complete & Sync"),
            Element::Code("br close ID\nbr sync  # Commit changes to git"),
            Element::Spacer(1),
            Element::Tip("Check br ready after each close - new work may have unblocked"),
        ],
    },
    Page {
        id: "workflow-bug-triage",
        title: "Triaging a Bug Report",
        section: "Workflows",
        elements: &[
            Element::Section("Efficient bug triage process"),
            Element::Spacer(1),
            Element::Section("Step 1: Create the Issue"),
            Element::Code(
                "br create --title=\"Login fails with special chars\" \\\n  --type=bug --priority=2",
            ),
            Element::Spacer(1),
            Element::Section("Step 2: Assess Severity"),
            Element::KeyTable(&[
                ("P0", "System down, data loss"),
                ("P1", "Major feature broken"),
                ("P2", "Feature degraded"),
                ("P3-P4", "Minor, cosmetic"),
            ]),
            Element::Spacer(1),
            Element::Section("Step 3: Add Labels"),
            Element::Paragraph(
                "Add labels such as bug, auth, user-reported with br, then press L in bvr to filter by them.",
            ),
            Element::Spacer(1),
            Element::Section("Step 4: Check for Blockers"),
            Element::Code("br dep add bv-feature1 bv-bug1  # Feature blocked by bug"),
            Element::Spacer(1),
            Element::Section("Checklist"),
            Element::Bullet(&[
                "Create issue with descriptive title",
                "Set priority based on severity",
                "Add relevant labels",
                "Check if it blocks other work",
            ]),
        ],
    },
    Page {
        id: "workflow-sprint-planning",
        title: "Sprint Planning Session",
        section: "Workflows",
        elements: &[
            Element::Section("Data-driven sprint decisions"),
            Element::Spacer(1),
            Element::Section("Step 1: Review Health"),
            Element::Paragraph(
                "Press i for Insights panel. Check open/blocked counts and top blockers.",
            ),
            Element::Spacer(1),
            Element::Section("Step 2: Identify Dependencies"),
            Element::Paragraph("Press g for graph view:"),
            Element::Bullet(&[
                "Tall chains = sequential (can't parallelize)",
                "Wide clusters = parallel opportunities",
                "Bottlenecks = single nodes blocking many",
            ]),
            Element::Spacer(1),
            Element::Section("Step 3: Filter to Ready Work"),
            Element::Paragraph("Press r to show only unblocked issues."),
            Element::Spacer(1),
            Element::Section("Step 4: Assign & Label"),
            Element::Paragraph(
                "For each sprint candidate, add a 'sprint-42' label with br, then press S for the sprint dashboard.",
            ),
            Element::Spacer(1),
            Element::Section("Step 5: Export Plan"),
            Element::Code("bvr --export-md sprint-42.md"),
        ],
    },
    Page {
        id: "workflow-onboarding",
        title: "Onboarding New Team Members",
        section: "Workflows",
        elements: &[
            Element::Section("Fast onboarding - it's in the repo"),
            Element::Spacer(1),
            Element::Section("Step 1: Clone & Run"),
            Element::Code("git clone <repo>\ncd project\nbvr  # Press ` for the tutorial"),
            Element::Paragraph("No separate tool installation. No access requests."),
            Element::Spacer(1),
            Element::Section("Step 2: Point to Help"),
            Element::KeyTable(&[
                ("?", "Quick reference overlay"),
                ("`", "Full interactive tutorial"),
                ("; / F2", "Shortcuts sidebar"),
            ]),
            Element::Spacer(1),
            Element::Section("Step 3: First Task"),
            Element::Paragraph("Find a good-first-issue: Press L, filter to that label."),
            Element::Spacer(1),
            Element::Section("Step 4: Walk Through Workflow"),
            Element::Bullet(&[
                "Find: filters (o/r) and search (/)",
                "Review: Enter for details, g for graph",
                "Claim: br update ID --status=in_progress",
                "Complete: br close ID && br sync",
            ]),
        ],
    },
    Page {
        id: "workflow-stakeholder-review",
        title: "Stakeholder Review",
        section: "Workflows",
        elements: &[
            Element::Section("Share with non-terminal users"),
            Element::Spacer(1),
            Element::Section("Generate Dashboard"),
            Element::Code(
                "bvr --pages  # Interactive wizard\n# Or direct:\nbvr --export-pages ./dashboard --pages-title \"Sprint 42\"",
            ),
            Element::Spacer(1),
            Element::Section("Output Includes"),
            Element::Bullet(&[
                "Triage recommendations",
                "Dependency graph visualization",
                "Full-text search",
                "Works offline after load",
            ]),
            Element::Spacer(1),
            Element::Section("Sharing Options"),
            Element::KeyTable(&[
                ("GitHub Pages", "Use wizard for auto-deploy"),
                ("Cloudflare", "Upload ./dashboard"),
                ("Email", "Zip and send"),
            ]),
            Element::Spacer(1),
            Element::Tip("Add to CI/CD to auto-update on each push"),
        ],
    },
    // =============================================================
    // REFERENCE (1 page)
    // =============================================================
    Page {
        id: "ref-keyboard",
        title: "Keyboard Reference",
        section: "Reference",
        elements: &[
            Element::Section("Global"),
            Element::KeyTable(&[
                ("?", "Help overlay"),
                ("`", "Tutorial"),
                ("q", "Quit"),
                ("Esc", "Close/back"),
                ("b/g/i/H", "Board / graph / insights / history"),
                ("E/S/t", "Tree / sprint / time travel"),
                ("[ / ] / f", "Label health / attention / flow"),
                ("!", "Alerts panel"),
            ]),
            Element::Spacer(1),
            Element::Section("Navigation"),
            Element::KeyTable(&[
                ("j / k", "Move down/up"),
                ("h / l", "Move left/right"),
                ("gg / G", "Top/bottom"),
                ("Ctrl+d / Ctrl+u", "Half-page down/up"),
                ("Tab", "Switch focus"),
                ("Enter", "Select"),
            ]),
            Element::Spacer(1),
            Element::Section("Filtering"),
            Element::KeyTable(&[
                ("/", "Search (n / N next/prev)"),
                ("o/c/r/a", "Status filter"),
                ("I / B", "In progress / blocked"),
                ("L", "Label picker"),
                ("'", "Recipe picker"),
            ]),
            Element::Spacer(1),
            Element::Section("Issue Actions"),
            Element::KeyTable(&[
                ("C", "Copy issue ID"),
                ("O", "Open in $EDITOR"),
                ("x", "Export issue markdown"),
                ("p", "Priority hints"),
            ]),
            Element::Spacer(1),
            Element::Tip("Press ? in any view for context-specific help"),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_have_unique_ids_titles_and_content() {
        let pages = pages();
        assert!(
            pages.len() >= 25,
            "expected the full tutorial, got {}",
            pages.len()
        );
        let mut ids = std::collections::HashSet::new();
        for page in pages {
            assert!(ids.insert(page.id), "duplicate page id {}", page.id);
            assert!(!page.title.is_empty() && !page.section.is_empty());
            assert!(!page.elements.is_empty(), "page {} is empty", page.id);
        }
    }

    #[test]
    fn content_uses_bvr_names_and_keys() {
        for page in pages() {
            for element in page.elements {
                if let Element::Code(code) = element {
                    for line in code.lines() {
                        let trimmed = line.trim_start();
                        assert!(
                            !trimmed.starts_with("bv "),
                            "page {} still runs legacy `bv`: {line}",
                            page.id
                        );
                    }
                }
            }
        }
    }
}
