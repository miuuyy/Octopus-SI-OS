const sessions = new Map();

module.exports = {
  schemaVersion: 1,
  async probe(ctx) {
    const account = sessions.get(ctx.interfaceId);
    return account ? { state: "connected", account } : { state: "not_connected" };
  },
  async connect(ctx) {
    const typed = Object.values(ctx.input).map((value) => value.trim()).filter(Boolean);
    const address = typed.find((value) => value.includes("@")) ?? "";
    if (typed.length > 0 && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(address)) {
      return { state: "error", message: "Enter a full email address, such as name@example.com." };
    }
    ctx.progress({
      message: "Approve the sign-in request in your browser, then return here.",
      url: "https://example.com/",
      code: "LUMN-4821",
    });
    await wait(2400, ctx.signal);
    if (Object.keys(ctx.input).length > 0) await ctx.saveValues(ctx.input);
    const account = { label: address || "Test account", detail: "Test profile" };
    sessions.set(ctx.interfaceId, account);
    return { state: "connected", account };
  },
  async disconnect(ctx) {
    sessions.delete(ctx.interfaceId);
    await ctx.saveValues(Object.fromEntries(Object.keys(ctx.values).map((name) => [name, null])));
    return { state: "not_connected" };
  },
};

function wait(milliseconds, signal) {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(signal.reason);
      return;
    }
    const timer = setTimeout(resolve, milliseconds);
    signal.addEventListener("abort", () => {
      clearTimeout(timer);
      reject(signal.reason);
    }, { once: true });
  });
}
