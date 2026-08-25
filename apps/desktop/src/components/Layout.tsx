import { Button, cn } from "@torben-app/ui";
import {
  Blocks,
  Boxes,
  CheckCircle2,
  CircleGauge,
  Command,
  Download,
  Library,
  type LucideIcon,
  PanelLeftClose,
  PanelLeftOpen,
  Search,
  Settings,
  Sparkles,
  X,
} from "lucide-react";
import { Dialog, Tooltip } from "radix-ui";
import { type ReactNode, useEffect, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { NavLink, useNavigate } from "react-router";
import type { ApplicationDescriptor } from "../types";

const navigation = [
  { to: "/overview", key: "overview", icon: CircleGauge },
  { to: "/catalog", key: "catalog", icon: Blocks },
  { to: "/installed", key: "installed", icon: Library },
  { to: "/tasks", key: "tasks", icon: Download },
  { to: "/plugins", key: "plugins", icon: Boxes },
  { to: "/diagnostics", key: "diagnostics", icon: CheckCircle2 },
  { to: "/settings", key: "settings", icon: Settings },
] as const;

const supportedApplicationRoutes = new Set(["node", "temurin", "python", "git", "vscode", "codex"]);

interface CommandItem {
  description: string;
  icon: LucideIcon;
  id: string;
  label: string;
  section: "applications" | "pages";
  searchable: string;
  to: string;
}

export function commandShortcut(platform: string) {
  const apple = /mac|iphone|ipad|ipod/i.test(platform);
  return apple ? { aria: "Meta+K", label: "⌘ K" } : { aria: "Control+K", label: "Ctrl K" };
}

export function Layout({
  applications,
  children,
}: {
  applications: ApplicationDescriptor[];
  children: ReactNode;
}) {
  const [collapsed, setCollapsed] = useState(false);
  const [commandOpen, setCommandOpen] = useState(false);
  const [commandQuery, setCommandQuery] = useState("");
  const [activeCommand, setActiveCommand] = useState(0);
  const { t } = useTranslation();
  const navigate = useNavigate();
  const commandListId = useId();
  const commandInput = useRef<HTMLInputElement>(null);
  const shortcut = commandShortcut(
    typeof navigator === "undefined" ? "" : navigator.platform || navigator.userAgent,
  );
  const commands = useMemo<CommandItem[]>(() => {
    const pages = navigation.map(({ to, key, icon }) => {
      const label = t(key);
      const description = t("layout.pageCommandDescription", { page: label });
      return {
        description,
        icon,
        id: `page-${key}`,
        label,
        section: "pages" as const,
        searchable: `${label} ${description}`.toLocaleLowerCase(),
        to,
      };
    });
    const applicationCommands = applications
      .filter((application) => supportedApplicationRoutes.has(application.id))
      .map((application) => ({
        description: t("layout.applicationCommandDescription", {
          application: application.displayName,
        }),
        icon: Boxes,
        id: `application-${application.id}`,
        label: application.displayName,
        section: "applications" as const,
        searchable: [
          application.id,
          application.displayName,
          application.summary,
          ...application.categories,
        ]
          .join(" ")
          .toLocaleLowerCase(),
        to: `/catalog/${application.id}`,
      }));
    return [...pages, ...applicationCommands];
  }, [applications, t]);
  const filteredCommands = useMemo(() => {
    const query = commandQuery.trim().toLocaleLowerCase();
    return query ? commands.filter((command) => command.searchable.includes(query)) : commands;
  }, [commandQuery, commands]);

  useEffect(() => {
    const openCommandPalette = (event: KeyboardEvent) => {
      if (!event.altKey && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setCommandOpen(true);
      }
    };
    window.addEventListener("keydown", openCommandPalette);
    return () => window.removeEventListener("keydown", openCommandPalette);
  }, []);

  const changeCommandOpen = (open: boolean) => {
    setCommandOpen(open);
    setActiveCommand(0);
    if (!open) {
      setCommandQuery("");
    }
  };

  const runCommand = (command: CommandItem) => {
    changeCommandOpen(false);
    navigate(command.to);
  };

  const handleCommandKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (!filteredCommands.length) {
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActiveCommand((current) => (current + 1) % filteredCommands.length);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActiveCommand(
        (current) => (current - 1 + filteredCommands.length) % filteredCommands.length,
      );
    } else if (event.key === "Home") {
      event.preventDefault();
      setActiveCommand(0);
    } else if (event.key === "End") {
      event.preventDefault();
      setActiveCommand(filteredCommands.length - 1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      const command = filteredCommands[activeCommand];
      if (command) {
        runCommand(command);
      }
    }
  };

  return (
    <Tooltip.Provider delayDuration={300}>
      <div className={cn("app-shell", collapsed && "sidebar-collapsed")}>
        <button
          className="skip-link"
          onClick={() => document.getElementById("main-content")?.focus()}
          type="button"
        >
          {t("layout.skipToContent")}
        </button>
        <aside className="sidebar">
          <div className="brand-row">
            <div className="brand-mark" aria-hidden="true">
              <Sparkles size={17} strokeWidth={2.3} />
            </div>
            <div className="brand-copy">
              <strong>Torben</strong>
              <span>App</span>
            </div>
            <Button
              aria-label={collapsed ? t("layout.expandSidebar") : t("layout.collapseSidebar")}
              className="sidebar-toggle"
              onClick={() => setCollapsed((value) => !value)}
              size="icon"
              variant="ghost"
            >
              {collapsed ? <PanelLeftOpen size={16} /> : <PanelLeftClose size={16} />}
            </Button>
          </div>

          <nav className="sidebar-nav" aria-label={t("layout.primaryNavigation")}>
            {navigation.map(({ to, key, icon: Icon }) => (
              <Tooltip.Root key={to}>
                <Tooltip.Trigger asChild>
                  <NavLink aria-label={t(key)} className="nav-item" to={to}>
                    <Icon size={17} />
                    <span>{t(key)}</span>
                  </NavLink>
                </Tooltip.Trigger>
                {collapsed ? (
                  <Tooltip.Portal>
                    <Tooltip.Content className="tooltip" side="right" sideOffset={8}>
                      {t(key)}
                    </Tooltip.Content>
                  </Tooltip.Portal>
                ) : null}
              </Tooltip.Root>
            ))}
          </nav>

          <div className="sidebar-footer">
            <div aria-hidden="true" className="status-dot" />
            <div>
              <strong>{t("layout.localCore")}</strong>
              <span>{t("layout.readyVersion")}</span>
            </div>
          </div>
        </aside>

        <div className="workspace">
          <header className="topbar" data-tauri-drag-region>
            <Dialog.Root onOpenChange={changeCommandOpen} open={commandOpen}>
              <Dialog.Trigger asChild>
                <button
                  aria-keyshortcuts={shortcut.aria}
                  aria-label={t("layout.search")}
                  className="command-search"
                  type="button"
                >
                  <Search size={15} />
                  <span>{t("layout.search")}</span>
                  <kbd>{shortcut.label}</kbd>
                </button>
              </Dialog.Trigger>
              <Dialog.Portal>
                <Dialog.Overlay className="dialog-overlay" />
                <Dialog.Content
                  className="dialog-content command-dialog"
                  onOpenAutoFocus={(event) => {
                    event.preventDefault();
                    commandInput.current?.focus();
                  }}
                >
                  <div className="command-dialog-header">
                    <div>
                      <Dialog.Title>{t("layout.commandPaletteTitle")}</Dialog.Title>
                      <Dialog.Description>
                        {t("layout.commandPaletteDescription")}
                      </Dialog.Description>
                    </div>
                    <Dialog.Close asChild>
                      <Button
                        aria-label={t("common.close")}
                        className="command-dialog-close"
                        size="icon"
                        variant="ghost"
                      >
                        <X size={16} />
                      </Button>
                    </Dialog.Close>
                  </div>
                  <div className="command-input-shell">
                    <Search aria-hidden="true" size={16} />
                    <input
                      aria-activedescendant={
                        filteredCommands[activeCommand]
                          ? `${commandListId}-${filteredCommands[activeCommand].id}`
                          : undefined
                      }
                      aria-autocomplete="list"
                      aria-controls={commandListId}
                      aria-expanded="true"
                      aria-label={t("layout.commandSearchLabel")}
                      onChange={(event) => {
                        setCommandQuery(event.target.value);
                        setActiveCommand(0);
                      }}
                      onKeyDown={handleCommandKeyDown}
                      placeholder={t("layout.commandSearchPlaceholder")}
                      ref={commandInput}
                      role="combobox"
                      value={commandQuery}
                    />
                    <kbd>{shortcut.label}</kbd>
                  </div>
                  <div className="command-results" id={commandListId} role="listbox">
                    {filteredCommands.length ? (
                      filteredCommands.map((command, index) => {
                        const Icon = command.icon;
                        return (
                          <button
                            aria-selected={index === activeCommand}
                            className={cn("command-result", index === activeCommand && "is-active")}
                            id={`${commandListId}-${command.id}`}
                            key={command.id}
                            onClick={() => runCommand(command)}
                            onMouseEnter={() => setActiveCommand(index)}
                            role="option"
                            tabIndex={-1}
                            type="button"
                          >
                            <span className="command-result-icon">
                              <Icon aria-hidden="true" size={16} />
                            </span>
                            <span className="command-result-copy">
                              <strong>{command.label}</strong>
                              <small>{command.description}</small>
                            </span>
                            <span className="command-result-section">
                              {t(`layout.commandSections.${command.section}`)}
                            </span>
                          </button>
                        );
                      })
                    ) : (
                      <div className="command-empty" role="status">
                        {t("layout.commandNoResults")}
                      </div>
                    )}
                  </div>
                  <div className="command-dialog-footer">
                    <span>{t("layout.commandNavigationHint")}</span>
                    <span>{t("layout.commandCloseHint")}</span>
                  </div>
                </Dialog.Content>
              </Dialog.Portal>
            </Dialog.Root>
            <div className="topbar-actions">
              <span className="local-badge">
                <Command size={13} />
                {t("layout.localFirst")}
              </span>
            </div>
          </header>
          <main className="content" id="main-content" tabIndex={-1}>
            {children}
          </main>
        </div>
      </div>
    </Tooltip.Provider>
  );
}
