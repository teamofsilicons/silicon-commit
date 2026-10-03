// Same-origin external script works with both the gateway and Vercel CSP.
(() => {
  const data = document.currentScript?.dataset;
  if (data?.retry === "true") {
    document.getElementById("retry")?.addEventListener("click", () => location.reload());
    return;
  }
  if (!window.opener || !data || !/^[a-f0-9-]{36}$/.test(data.attempt || "") || !["carbon", "silicon"].includes(data.kind || "")) return;
  window.opener.postMessage({ type: "commit:sign-in", attempt: data.attempt, kind: data.kind, ok: data.ok === "true", contextId: data.context }, location.origin);
  window.close();
})();
