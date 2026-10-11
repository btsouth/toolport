export const SETTINGS_PAGES = [
  { id: "general", label: "General", description: "Startup, appearance and updates." },
  { id: "tools", label: "Tools", description: "How agents find and use your tools." },
  {
    id: "safety",
    label: "Safety",
    description: "How Toolport handles risky tool calls.",
  },
  {
    id: "access",
    label: "Access",
    description: "Limit which servers and tools each client can use.",
  },
  {
    id: "connections",
    label: "Connections",
    description: "Your local HTTP endpoint and gateway processes.",
  },
  {
    id: "help",
    label: "Help and data",
    description: "Support reports, local files and removing Toolport.",
  },
] as const;
export type SettingsSubpage = (typeof SETTINGS_PAGES)[number]["id"];
