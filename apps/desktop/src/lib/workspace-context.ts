import { createContext, useContext } from "react";

import type { Workspace } from "./use-workspace";

/**
 * One shared [`Workspace`] for the whole app. The backing state machine
 * (`useWorkspaceState`) runs a single IPC poll loop and a single connection state machine,
 * so it must be instantiated exactly once (by `WorkspaceProvider`). Every section reads
 * the same instance through this context instead of calling the hook itself — otherwise
 * each mounted section would start its own poll loop and keep a separate, drifting copy of
 * the connection.
 */
export const WorkspaceContext = createContext<Workspace | null>(null);

/** Read the shared workspace. Throws if used outside a `WorkspaceProvider`. */
export function useWorkspace(): Workspace {
  const workspace = useContext(WorkspaceContext);
  if (workspace === null) {
    throw new Error("useWorkspace must be used within a WorkspaceProvider");
  }
  return workspace;
}
