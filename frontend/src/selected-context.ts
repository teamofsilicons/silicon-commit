/** Public context IDs only. Authentication stays in sealed HttpOnly cookies. */
export const selectionKey = (scope: string) => "commit.context." + scope;
export function selectedContext(scope: string): string {
  try {
    const key = selectionKey(scope),
      value = sessionStorage.getItem(key);
    if (value === null) {
      sessionStorage.setItem(key, "none");
      return "none";
    }
    if (value === "none" || /^[a-f0-9]{32}$/.test(value)) return value;
  } catch {
    throw new Error(
      "Allow session storage in this tab to select an account safely.",
    );
  }
  throw new Error(
    "This tab's saved account is invalid. Open Commit in a new tab to choose it again.",
  );
}
export function saveSelectedContext(scope: string, id?: string) {
  if (id !== undefined && !/^[a-f0-9]{32}$/.test(id))
    throw new Error("The selected account was not verified.");
  try {
    sessionStorage.setItem(selectionKey(scope), id || "none");
  } catch {
    throw new Error(
      "Allow session storage in this tab to select an account safely.",
    );
  }
}
