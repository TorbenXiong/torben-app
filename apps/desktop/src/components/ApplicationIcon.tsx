const applicationIconAssets: Record<string, string> = {
  node: "/icons/nodejs.svg",
  temurin: "/icons/duke.png",
  python: "/icons/python.svg",
  rust: "/icons/rust.svg",
  mysql: "/icons/mysql-logo.png",
  redis: "/icons/redis-mark.svg",
  postgresql: "/icons/postgresql.svg",
  git: "/icons/git.svg",
  vscode: "/icons/vscode.svg",
  codex: "/icons/openai.svg",
};

export function ApplicationIcon({
  className,
  id,
  size = 18,
}: {
  className?: string;
  id: string;
  size?: number;
}) {
  return (
    <img
      alt=""
      aria-hidden="true"
      className={["application-icon", `application-icon-${id}`, className]
        .filter(Boolean)
        .join(" ")}
      height={size}
      src={applicationIconAssets[id]}
      width={size}
    />
  );
}

export function NodeIcon({ size = 17 }: { size?: number }) {
  return <ApplicationIcon className="app-nav-icon" id="node" size={size} />;
}

export function JavaIcon({ size = 17 }: { size?: number }) {
  return <ApplicationIcon className="app-nav-icon java-nav-icon" id="temurin" size={size} />;
}

export function PythonIcon({ size = 17 }: { size?: number }) {
  return <ApplicationIcon className="app-nav-icon" id="python" size={size} />;
}

export function RustIcon({ size = 17 }: { size?: number }) {
  return <ApplicationIcon className="app-nav-icon" id="rust" size={size} />;
}

export function MysqlIcon({ size = 17 }: { size?: number }) {
  return <ApplicationIcon className="app-nav-icon" id="mysql" size={size} />;
}

export function RedisIcon({ size = 17 }: { size?: number }) {
  return <ApplicationIcon className="app-nav-icon" id="redis" size={size} />;
}

export function PostgresqlIcon({ size = 17 }: { size?: number }) {
  return <ApplicationIcon className="app-nav-icon" id="postgresql" size={size} />;
}
