import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { HashRouter } from "react-router";
import { afterEach, expect, it, vi } from "vitest";
import App from "../App";
import * as api from "../api";
import i18n from "../i18n";
import type { InstallRecord, OperationEvent } from "../types";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const runtimes = [
  { appId: "node", route: "/node", version: "24.19.0" },
  { appId: "temurin", route: "/java", version: "21.0.2+13.0.LTS" },
  { appId: "python", route: "/python", version: "3.14.7" },
  { appId: "rust", route: "/rust", version: "1.98.0" },
  { appId: "mysql", route: "/mysql", version: "8.4.11" },
  { appId: "redis", route: "/redis", version: "8.8.0" },
  { appId: "postgresql", route: "/postgresql", version: "18.6.0" },
];

async function prepareApp(events: OperationEvent[] = [], installed: InstallRecord[] = []) {
  await i18n.changeLanguage("en");
  const snapshot = await api.getSnapshot();
  vi.spyOn(api, "getSnapshot").mockResolvedValue({
    ...snapshot,
    operations: events,
    installed,
    selected: [],
    warnings: [],
    plugins: snapshot.plugins.map((plugin) => ({ ...plugin, enabled: true })),
    settings: {
      ...snapshot.settings,
      language: "en",
      updates: {
        ...snapshot.settings.updates,
        notifyManagedApps: false,
        automaticallyUpdateApps: [],
      },
    },
  });
  vi.spyOn(api, "getOperationEvents").mockResolvedValue(events);
  vi.spyOn(api, "onVersionCatalogUpdated").mockResolvedValue(() => undefined);
  vi.spyOn(api, "getVersions").mockImplementation(async (appId) => [
    {
      version: runtimes.find((runtime) => runtime.appId === appId)?.version ?? "1.0.0",
      releasedAt: "2026-08-01",
      ltsName: "LTS",
      recommended: true,
    },
  ]);
}

function renderApp(route: string) {
  window.location.hash = `#${route}`;
  render(
    <HashRouter>
      <App />
    </HashRouter>,
  );
}

function installation(appId: string, version: string): InstallRecord {
  return {
    appId,
    version,
    sourceId: `${appId}.official`,
    scope: "managed",
    installPath: `D:/TorbenApp/userData/apps/${appId}/${version}`,
    installedAt: "fixture",
    health: "healthy",
  };
}

function navigate(plugin: string) {
  fireEvent.click(
    within(screen.getByRole("navigation", { name: "Primary navigation" })).getByRole("link", {
      name: plugin,
    }),
  );
}

it.each(runtimes)(
  "retains only the inline $appId progress after leaving and returning",
  async ({ appId, route, version }) => {
    await prepareApp();
    let complete!: (record: InstallRecord) => void;
    const install = vi.spyOn(api, "installApp").mockReturnValue(
      new Promise((resolve) => {
        complete = resolve;
      }),
    );
    renderApp(route);
    fireEvent.click(await screen.findByRole("button", { name: "Install" }));
    expect(install).toHaveBeenCalledExactlyOnceWith(appId, version);
    expect(screen.getAllByRole("progressbar")).toHaveLength(1);
    expect(screen.queryByRole("region", { name: "Active operations" })).not.toBeInTheDocument();
    navigate(appId === "node" ? "Python" : "Node.js");
    await screen.findByRole("button", { name: "Install" });
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    navigate(
      appId === "node"
        ? "Node.js"
        : appId === "temurin"
          ? "Java"
          : appId === "postgresql"
            ? "PostgreSQL"
            : appId === "mysql"
              ? "MySQL"
              : appId === "redis"
                ? "Redis"
                : appId === "rust"
                  ? "Rust"
                  : "Python",
    );
    expect(await screen.findByRole("button", { name: "Installing…" })).toBeDisabled();
    expect(screen.getAllByRole("progressbar")).toHaveLength(1);
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "2");
    await act(async () => {
      complete(installation(appId, version));
    });
    await waitFor(() => expect(screen.queryByRole("progressbar")).not.toBeInTheDocument());
  },
);

it("retains inline uninstall progress after switching plugins", async () => {
  await prepareApp([], [installation("node", "24.19.0")]);
  let complete!: () => void;
  vi.spyOn(api, "uninstallApp").mockReturnValue(
    new Promise((resolve) => {
      complete = resolve;
    }),
  );
  renderApp("/node");
  fireEvent.click(await screen.findByRole("button", { name: "Uninstall" }));
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Uninstall" }));
  expect(screen.getAllByRole("progressbar")).toHaveLength(1);
  navigate("Python");
  await screen.findByRole("button", { name: "Install" });
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  navigate("Node.js");
  expect(await screen.findByRole("button", { name: "Uninstalling…" })).toBeDisabled();
  await act(async () => {
    complete();
  });
  await waitFor(() => expect(screen.queryByRole("progressbar")).not.toBeInTheDocument());
});

it("keeps a failure on its own plugin after navigating away and permits retry", async () => {
  await prepareApp();
  let fail!: (reason: unknown) => void;
  const install = vi.spyOn(api, "installApp").mockReturnValueOnce(
    new Promise((_resolve, reject) => {
      fail = reject;
    }),
  );
  renderApp("/python");
  fireEvent.click(await screen.findByRole("button", { name: "Install" }));
  navigate("Node.js");
  await act(async () => {
    fail({ code: "python_network_error", message: "Python request timed out" });
  });
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(await screen.findByRole("button", { name: "Install" })).toBeEnabled();
  navigate("Python");
  expect(await screen.findByRole("alert")).toHaveTextContent("python_network_error");
  expect(await screen.findByRole("button", { name: "Install" })).toBeEnabled();
  install.mockResolvedValueOnce(installation("python", "3.14.7"));
  fireEvent.click(screen.getByRole("button", { name: "Install" }));
  await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
  expect(install).toHaveBeenCalledTimes(2);
});

it("does not cancel another installation when Python fails and refreshes its committed record", async () => {
  await prepareApp();
  let completeNode!: (record: InstallRecord) => void;
  let failPython!: (error: Error) => void;
  vi.spyOn(api, "installApp").mockImplementation((appId) =>
    appId === "node"
      ? new Promise((resolve) => {
          completeNode = resolve;
        })
      : new Promise((_resolve, reject) => {
          failPython = reject;
        }),
  );
  renderApp("/node");
  fireEvent.click(await screen.findByRole("button", { name: "Install" }));
  navigate("Python");
  fireEvent.click(await screen.findByRole("button", { name: "Install" }));
  navigate("Node.js");
  expect(await screen.findByRole("button", { name: "Installing…" })).toBeDisabled();
  await act(async () => {
    failPython(new Error("Python metadata fixture failed"));
  });
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Installing…" })).toBeDisabled();
  const snapshot = await api.getSnapshot();
  const record = installation("node", "24.19.0");
  vi.mocked(api.getSnapshot).mockResolvedValue({ ...snapshot, installed: [record] });
  await act(async () => {
    completeNode(record);
  });
  expect(await screen.findByRole("button", { name: "Uninstall" })).toBeEnabled();
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  navigate("Python");
  expect(await screen.findByRole("alert")).toHaveTextContent("Python metadata fixture failed");
  expect(await screen.findByRole("button", { name: "Install" })).toBeEnabled();
});

it.each(["succeeded", "failed", "rolled_back"] as const)(
  "polls inline progress and removes a %s task without affecting another plugin",
  async (state) => {
    const node: OperationEvent = {
      operationId: "node-fixture",
      sequence: 2,
      state: "running",
      phase: "download",
      message: "Downloading Node archive",
      progress: 0.42,
      timestamp: "1700000000",
      kind: "install",
      appId: "node",
      version: "24.19.0",
    };
    const python: OperationEvent = {
      ...node,
      operationId: "python-fixture",
      appId: "python",
      version: "3.14.7",
      progress: 0.7,
    };
    await prepareApp([node, python]);
    let events = [node, python];
    vi.mocked(api.getOperationEvents).mockImplementation(async () => events);
    vi.useFakeTimers();
    await act(async () => {
      renderApp("/node");
    });
    expect(screen.getAllByRole("progressbar")).toHaveLength(1);
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "42");
    events = [node, python, { ...node, sequence: 3, state: "cancelling", progress: 0.5 }];
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "50");
    events = [{ ...node, sequence: 4, state, progress: 1 }, python, node];
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    await act(async () => {
      navigate("Python");
    });
    expect(screen.getAllByRole("progressbar")).toHaveLength(1);
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "70");
  },
);
