import { getCurrentWindow } from "@tauri-apps/api/window";
import { Button, cn } from "@torben-app/ui";
import {
  ArrowLeft,
  ArrowRight,
  Boxes,
  CheckCircle2,
  Command,
  Minus,
  PanelLeftClose,
  PanelLeftOpen,
  ScrollText,
  Search,
  Settings,
  Sparkles,
  Square,
  X,
} from "lucide-react";
import { Dialog, DropdownMenu, Tooltip } from "radix-ui";
import {
  type ComponentType,
  type ReactNode,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { NavLink, useNavigate } from "react-router";
import type { ApplicationDescriptor, PluginSummary } from "../types";
import {
  JavaIcon,
  MysqlIcon,
  NodeIcon,
  PostgresqlIcon,
  PythonIcon,
  RedisIcon,
  RustIcon,
} from "./ApplicationIcon";

const primaryNavigation = [{ to: "/plugins", key: "plugins", icon: Boxes }] as const;

const logsNavigation = { to: "/logs", key: "logs", icon: ScrollText };
const diagnosticsNavigation = { to: "/diagnostics", key: "diagnostics", icon: CheckCircle2 };
const settingsNavigation = { to: "/settings", key: "settings", icon: Settings };
const appVersion = "0.1.0";

const supportedApplicationRoutes = new Set([
  "node",
  "temurin",
  "python",
  "rust",
  "mysql",
  "redis",
  "postgresql",
  "git",
  "vscode",
  "codex",
]);

interface CommandItem {
  description: string;
  icon: ComponentType<{ size?: number }>;
  id: string;
  label: string;
  section: "applications" | "pages";
  searchable: string;
  to: string;
}

interface NavigationItem {
  icon: ComponentType<{ size?: number }>;
  key: string;
  to: string;
}

async function performWindowAction(
  action: (appWindow: ReturnType<typeof getCurrentWindow>) => Promise<void>,
) {
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
    return;
  }
  await action(getCurrentWindow());
}

function SidebarLink({
  child = false,
  collapsed,
  item,
  label,
}: {
  child?: boolean;
  collapsed: boolean;
  item: NavigationItem;
  label: string;
}) {
  const Icon = item.icon;
  return (
    <Tooltip.Root>
      <Tooltip.Trigger asChild>
        <NavLink
          aria-label={label}
          className={cn("nav-item", child && "nav-item-child")}
          to={item.to}
        >
          <Icon size={child ? 15 : 16} />
          <span>{label}</span>
        </NavLink>
      </Tooltip.Trigger>
      {collapsed ? (
        <Tooltip.Portal>
          <Tooltip.Content className="tooltip" side="right" sideOffset={8}>
            {label}
          </Tooltip.Content>
        </Tooltip.Portal>
      ) : null}
    </Tooltip.Root>
  );
}

export function commandShortcut(platform: string) {
  const apple = /mac|iphone|ipad|ipod/i.test(platform);
  return apple ? { aria: "Meta+K", label: "⌘ K" } : { aria: "Control+K", label: "Ctrl K" };
}

export function Layout({
  applications,
  children,
  plugins,
}: {
  applications: ApplicationDescriptor[];
  children: ReactNode;
  plugins: PluginSummary[];
}) {
  const [collapsed, setCollapsed] = useState(false);
  const [commandOpen, setCommandOpen] = useState(false);
  const [commandQuery, setCommandQuery] = useState("");
  const [activeCommand, setActiveCommand] = useState(0);
  const [aboutOpen, setAboutOpen] = useState(false);
  const { t } = useTranslation();
  const navigate = useNavigate();
  const commandListId = useId();
  const commandInput = useRef<HTMLInputElement>(null);
  const shortcut = commandShortcut(
    typeof navigator === "undefined" ? "" : navigator.platform || navigator.userAgent,
  );
  const temurinEnabled = plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.temurin" && plugin.enabled,
  );
  const nodeEnabled = plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.node" && plugin.enabled,
  );
  const pythonEnabled = plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.python" && plugin.enabled,
  );
  const rustEnabled = plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.rust" && plugin.enabled,
  );
  const mysqlEnabled = plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.mysql" && plugin.enabled,
  );
  const redisEnabled = plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.redis" && plugin.enabled,
  );
  const postgresqlEnabled = plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.postgresql" && plugin.enabled,
  );
  const runtimePages = useMemo<NavigationItem[]>(() => {
    const runtimePages = [];
    if (nodeEnabled) runtimePages.push({ to: "/node", key: "node", icon: NodeIcon });
    if (temurinEnabled) {
      runtimePages.push({ to: "/java", key: "java", icon: JavaIcon });
    }
    if (pythonEnabled) {
      runtimePages.push({ to: "/python", key: "python", icon: PythonIcon });
    }
    if (rustEnabled) {
      runtimePages.push({ to: "/rust", key: "rust", icon: RustIcon });
    }
    if (mysqlEnabled) {
      runtimePages.push({ to: "/mysql", key: "mysql", icon: MysqlIcon });
    }
    if (redisEnabled) {
      runtimePages.push({ to: "/redis", key: "redis", icon: RedisIcon });
    }
    if (postgresqlEnabled) {
      runtimePages.push({ to: "/postgresql", key: "postgresql", icon: PostgresqlIcon });
    }
    return runtimePages;
  }, [
    mysqlEnabled,
    nodeEnabled,
    postgresqlEnabled,
    pythonEnabled,
    redisEnabled,
    rustEnabled,
    temurinEnabled,
  ]);
  const navigation = useMemo(
    () => [
      ...primaryNavigation,
      ...runtimePages,
      logsNavigation,
      diagnosticsNavigation,
      settingsNavigation,
    ],
    [runtimePages],
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
      .filter(
        (application) =>
          application.capabilities.length > 0 &&
          supportedApplicationRoutes.has(application.id) &&
          (application.id !== "temurin" || temurinEnabled) &&
          (application.id !== "python" || pythonEnabled) &&
          (application.id !== "node" || nodeEnabled) &&
          (application.id !== "rust" || rustEnabled) &&
          (application.id !== "mysql" || mysqlEnabled) &&
          (application.id !== "redis" || redisEnabled) &&
          (application.id !== "postgresql" || postgresqlEnabled),
      )
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
        to:
          application.id === "temurin"
            ? "/java"
            : application.id === "python"
              ? "/python"
              : application.id === "node"
                ? "/node"
                : application.id === "rust"
                  ? "/rust"
                  : application.id === "mysql"
                    ? "/mysql"
                    : application.id === "redis"
                      ? "/redis"
                      : application.id === "postgresql"
                        ? "/postgresql"
                        : "/plugins",
      }));
    return [...pages, ...applicationCommands];
  }, [
    applications,
    mysqlEnabled,
    navigation,
    nodeEnabled,
    postgresqlEnabled,
    pythonEnabled,
    redisEnabled,
    rustEnabled,
    t,
    temurinEnabled,
  ]);
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
        <header className="window-titlebar">
          <div className="titlebar-navigation">
            <Tooltip.Root>
              <Tooltip.Trigger asChild>
                <button
                  aria-label={collapsed ? t("layout.expandSidebar") : t("layout.collapseSidebar")}
                  className="titlebar-tool"
                  onClick={() => setCollapsed((value) => !value)}
                  type="button"
                >
                  {collapsed ? <PanelLeftOpen size={15} /> : <PanelLeftClose size={15} />}
                </button>
              </Tooltip.Trigger>
              <Tooltip.Portal>
                <Tooltip.Content className="tooltip" side="bottom" sideOffset={6}>
                  {collapsed ? t("layout.expandSidebar") : t("layout.collapseSidebar")}
                </Tooltip.Content>
              </Tooltip.Portal>
            </Tooltip.Root>
            <Tooltip.Root>
              <Tooltip.Trigger asChild>
                <button
                  aria-label={t("layout.goBack")}
                  className="titlebar-tool"
                  onClick={() => navigate(-1)}
                  type="button"
                >
                  <ArrowLeft size={15} />
                </button>
              </Tooltip.Trigger>
              <Tooltip.Portal>
                <Tooltip.Content className="tooltip" side="bottom" sideOffset={6}>
                  {t("layout.goBack")}
                </Tooltip.Content>
              </Tooltip.Portal>
            </Tooltip.Root>
            <Tooltip.Root>
              <Tooltip.Trigger asChild>
                <button
                  aria-label={t("layout.goForward")}
                  className="titlebar-tool"
                  onClick={() => navigate(1)}
                  type="button"
                >
                  <ArrowRight size={15} />
                </button>
              </Tooltip.Trigger>
              <Tooltip.Portal>
                <Tooltip.Content className="tooltip" side="bottom" sideOffset={6}>
                  {t("layout.goForward")}
                </Tooltip.Content>
              </Tooltip.Portal>
            </Tooltip.Root>
            <DropdownMenu.Root>
              <DropdownMenu.Trigger asChild>
                <button className="titlebar-help-trigger" type="button">
                  {t("layout.help")}
                </button>
              </DropdownMenu.Trigger>
              <DropdownMenu.Portal>
                <DropdownMenu.Content align="start" className="help-menu-content" sideOffset={2}>
                  <DropdownMenu.Item
                    className="help-menu-item"
                    onSelect={() => navigate(diagnosticsNavigation.to)}
                  >
                    {t(diagnosticsNavigation.key)}
                  </DropdownMenu.Item>
                  <DropdownMenu.Item
                    className="help-menu-item"
                    onSelect={() => navigate(logsNavigation.to)}
                  >
                    {t(logsNavigation.key)}
                  </DropdownMenu.Item>
                  <DropdownMenu.Separator className="help-menu-separator" />
                  <DropdownMenu.Item className="help-menu-item" onSelect={() => setAboutOpen(true)}>
                    {t("layout.about")}
                  </DropdownMenu.Item>
                </DropdownMenu.Content>
              </DropdownMenu.Portal>
            </DropdownMenu.Root>
          </div>
          <div className="titlebar-drag-region" data-tauri-drag-region />
          <div className="window-controls">
            <button
              aria-label={t("layout.minimizeWindow")}
              className="window-control"
              onClick={() => {
                void performWindowAction((appWindow) => appWindow.minimize());
              }}
              type="button"
            >
              <Minus size={14} />
            </button>
            <button
              aria-label={t("layout.maximizeWindow")}
              className="window-control"
              onClick={() => {
                void performWindowAction((appWindow) => appWindow.toggleMaximize());
              }}
              type="button"
            >
              <Square size={11} />
            </button>
            <button
              aria-label={t("layout.closeWindow")}
              className="window-control window-close"
              onClick={() => {
                void performWindowAction((appWindow) => appWindow.close());
              }}
              type="button"
            >
              <X size={15} />
            </button>
          </div>
        </header>

        <Dialog.Root onOpenChange={setAboutOpen} open={aboutOpen}>
          <Dialog.Portal>
            <Dialog.Overlay className="dialog-overlay" />
            <Dialog.Content className="dialog-content about-dialog">
              <div className="about-dialog-header">
                <div className="about-mark" aria-hidden="true">
                  <Sparkles size={22} strokeWidth={2.3} />
                </div>
                <div>
                  <Dialog.Title>{t("layout.aboutTitle")}</Dialog.Title>
                  <span className="about-version">
                    {t("layout.aboutVersion", { version: appVersion })}
                  </span>
                </div>
                <Dialog.Close asChild>
                  <Button
                    aria-label={t("common.close")}
                    className="about-dialog-close"
                    size="icon"
                    variant="ghost"
                  >
                    <X size={16} />
                  </Button>
                </Dialog.Close>
              </div>
              <Dialog.Description className="about-description">
                {t("layout.aboutDescription")}
              </Dialog.Description>
              <p className="about-privacy">{t("layout.aboutPrivacy")}</p>
              <div className="dialog-actions about-actions">
                <Dialog.Close asChild>
                  <Button variant="secondary">{t("common.close")}</Button>
                </Dialog.Close>
              </div>
            </Dialog.Content>
          </Dialog.Portal>
        </Dialog.Root>

        <aside className="sidebar">
          <div className="brand-row">
            <div className="brand-mark" aria-hidden="true">
              <Sparkles size={17} strokeWidth={2.3} />
            </div>
            <div className="brand-copy">
              <strong>Torben</strong>
              <span>App</span>
            </div>
          </div>

          <nav className="sidebar-nav" aria-label={t("layout.primaryNavigation")}>
            <div className="sidebar-primary-nav">
              {primaryNavigation.map((item) => (
                <SidebarLink collapsed={collapsed} item={item} key={item.to} label={t(item.key)} />
              ))}
              {runtimePages.length ? (
                <div className="installed-plugin-nav">
                  <span className="nav-section-label">{t("layout.installedPlugins")}</span>
                  {runtimePages.map((item) => (
                    <SidebarLink
                      child
                      collapsed={collapsed}
                      item={item}
                      key={item.to}
                      label={t(item.key)}
                    />
                  ))}
                </div>
              ) : null}
            </div>
          </nav>

          <div className="sidebar-footer">
            <SidebarLink
              collapsed={collapsed}
              item={settingsNavigation}
              label={t(settingsNavigation.key)}
            />
          </div>
        </aside>

        <div className="workspace">
          <header className="topbar" data-tauri-drag-region>
            <div className="topbar-context" aria-hidden="true">
              <strong>Torben App</strong>
              <span>/</span>
              <span>{t("layout.localWorkspace")}</span>
            </div>
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
