import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useMemo,
  useRef,
  useState,
} from "react";
import { formatTorbenError } from "../api";

interface PendingRuntimeOperation {
  id: number;
  appId: string;
  version: string;
  kind: "install" | "uninstall";
}

interface RuntimeOperationsState {
  pending: PendingRuntimeOperation[];
  errors: Record<string, string>;
  run: (appId: string, action: string, operation: () => Promise<unknown>) => Promise<unknown>;
}

const RuntimeOperationsContext = createContext<RuntimeOperationsState>({
  pending: [],
  errors: {},
  run: (_appId, _action, operation) => operation(),
});

export function RuntimeOperationsProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<PendingRuntimeOperation[]>([]);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const nextId = useRef(0);
  const run = useCallback(
    async (appId: string, action: string, operation: () => Promise<unknown>) => {
      const [kind, version] = action.split(":");
      if ((kind !== "install" && kind !== "uninstall") || !version) return operation();
      const id = nextId.current++;
      setPending((current) => [...current, { id, appId, version, kind }]);
      setErrors((current) => {
        const next = { ...current };
        delete next[appId];
        return next;
      });
      try {
        return await operation();
      } catch (reason) {
        setErrors((current) => ({ ...current, [appId]: formatTorbenError(reason) }));
        throw reason;
      } finally {
        setPending((current) => current.filter((item) => item.id !== id));
      }
    },
    [],
  );
  const value = useMemo(() => ({ pending, errors, run }), [pending, errors, run]);
  return (
    <RuntimeOperationsContext.Provider value={value}>{children}</RuntimeOperationsContext.Provider>
  );
}

export function useRuntimeOperations() {
  const { pending, errors, run } = useContext(RuntimeOperationsContext);
  return {
    run,
    errorFor: (appId: string) => errors[appId],
    isPending: (appId: string, kind: "install" | "uninstall", version: string) =>
      pending.some(
        (item) => item.appId === appId && item.kind === kind && item.version === version,
      ),
  };
}
