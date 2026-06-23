import { type ReactElement, type ReactNode } from "react";

import { useWorkspaceState } from "./use-workspace";
import { WorkspaceContext } from "./workspace-context";

/**
 * Mount once at the app root; instantiates the single backing workspace state machine and
 * provides it to all descendants via [`WorkspaceContext`]. Sections read it with the
 * `useWorkspace` hook.
 */
export function WorkspaceProvider({
  children,
}: {
  children: ReactNode;
}): ReactElement {
  const workspace = useWorkspaceState();
  return (
    <WorkspaceContext.Provider value={workspace}>
      {children}
    </WorkspaceContext.Provider>
  );
}
