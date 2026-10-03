// Same-origin external script works with both the gateway and Vercel CSP.
(() => {
  const data = document.currentScript?.dataset;
  // Callback pages never select an account. Activation is a short, correlated
  // request checked against the browser's latest login and selected context.
  if (data?.page === "true") {
    history.replaceState(null, "", "/auth/callback");
    const status = document.getElementById("login-status");
    const retry = document.getElementById("retry-activation");
    const key = "commit.context.production";
    let previous;
    try {
      previous = sessionStorage.getItem(key) || "none";
      sessionStorage.setItem(key, previous);
    } catch {
      status.textContent =
        "Allow session storage in this tab to select an account safely.";
      return;
    }
    const finish = async () => {
      retry.hidden = true;
      try {
        const response = await fetch("/auth/activate", {
          method: "POST",
          credentials: "same-origin",
          headers: {
            "content-type": "application/json",
            "x-commit-environment": "production",
            "x-commit-context": previous,
          },
          body: JSON.stringify({ context_id: data.context, state: data.state }),
        });
        const result = await response.json();
        if (
          !response.ok ||
          result.authenticated !== true ||
          result.context_id !== data.context ||
          result.actor?.type !== data.kind
        )
          throw new Error(
            result.error?.message ||
              "This sign-in could not be verified. Return to Commit and try again.",
          );
        const destination = decodeURIComponent(data.return || "");
        if (
          !/^\/#\/(todos|projects|notifications|environments)(?:[/?][^\\\r\n]*)?$/.test(
            destination,
          )
        )
          throw new Error("Return to Commit to continue.");
        if ((sessionStorage.getItem(key) || "none") !== previous)
          throw new Error("The selected workspace changed. Return to Commit.");
        sessionStorage.setItem(key, result.context_id);
        location.replace(destination);
      } catch (error) {
        status.textContent =
          error instanceof Error
            ? error.message
            : "Sign-in could not finish. Retry or return to Commit.";
        retry.hidden = false;
      }
    };
    retry.addEventListener("click", finish);
    void finish();
    return;
  }
  if (data?.retry === "true") {
    document
      .getElementById("retry")
      ?.addEventListener("click", () => location.reload());
    return;
  }
  if (
    !window.opener ||
    !data ||
    !/^[a-f0-9-]{36}$/.test(data.attempt || "") ||
    !["carbon", "silicon"].includes(data.kind || "")
  )
    return;
  window.opener.postMessage(
    {
      type: "commit:sign-in",
      attempt: data.attempt,
      kind: data.kind,
      ok: data.ok === "true",
      contextId: data.context,
    },
    location.origin,
  );
  window.close();
})();
