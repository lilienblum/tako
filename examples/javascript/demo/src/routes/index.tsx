import { createFileRoute } from "@tanstack/react-router";
import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { MessageCircle, Radio, Send } from "lucide-react";
import { Tako } from "tako.sh";
import { useChannel } from "tako.sh/react";
import { useMemo, useRef, useState } from "react";

// ── Server functions ─────────────────────────────────────────────────────────

const getPageData = createServerFn().handler(async () => {
  const request = getRequest();
  const host = (request?.headers.get("host") ?? "").split(":")[0] ?? "";
  const labels = host.split(".");
  const demoIdx = labels.indexOf("demo");
  const tenant = demoIdx === 1 ? (labels[0] ?? null) : null;

  return { tenant };
});

const enqueueBroadcast = createServerFn()
  .inputValidator((data: { id: string; message: string }) => data)
  .handler(async ({ data }) => {
    await Tako.workflows.enqueue("broadcast", data);
  });

// ── Route ────────────────────────────────────────────────────────────────────

export const Route = createFileRoute("/")({
  loader: () => getPageData(),
  component: Home,
});

// ── Component ────────────────────────────────────────────────────────────────

function Home() {
  const { tenant } = Route.useLoaderData();
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [pending, setPending] = useState<Set<string>>(() => new Set());
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const { messages: channelMessages } = useChannel<{ id: string; message: string }>(
    "demo-broadcast",
    {
      onMessage: (msg) => {
        const id = msg.data.id;
        setPending((prev) => {
          if (!prev.has(id)) return prev;
          const next = new Set(prev);
          next.delete(id);
          return next;
        });
      },
    },
  );

  // Newest-first for display.
  const messages = useMemo(
    () =>
      channelMessages
        .map((m) => ({ id: m.data.id, text: m.data.message }))
        .slice()
        .reverse(),
    [channelMessages],
  );

  async function handleSend() {
    const text = input.trim();
    if (!text || sending) return;
    const id = crypto.randomUUID();
    setSending(true);
    setInput("");
    setError(null);
    try {
      await enqueueBroadcast({ data: { id, message: text } });
      setPending((prev) => new Set(prev).add(id));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSending(false);
      inputRef.current?.focus();
    }
  }

  return (
    <div className="page">
      <header className="page-header">
        <div className="header-inner">
          <a className="logo" href="https://tako.sh">
            <img src="/favicon.svg" alt="" aria-hidden="true" width="28" height="28" />
            <span>tako</span>
            <span className="logo-label">demo</span>
          </a>
          <div className="header-right">
            {tenant && (
              <div className="tenant-tag">
                <span className="tenant-tag-label">tenant</span>
                <span className="tenant-tag-value">{tenant}</span>
              </div>
            )}
            <a
              className="header-source"
              href="https://github.com/lilienblum/tako/tree/master/examples/javascript/demo"
              target="_blank"
              rel="noopener noreferrer"
            >
              <img
                src="https://cdn.simpleicons.org/github/2f2a44"
                alt=""
                aria-hidden="true"
                width={15}
                height={15}
              />
              <span>source</span>
            </a>
          </div>
        </div>
      </header>

      <main className="page-main">
        <div className="page-title">
          <h1>Channels + workflows</h1>
          <p>A live Tako demo — durable pub/sub meets server-driven workflows.</p>
        </div>

        <div className="card">
          <div className="card-head">
            <Radio size={14} className="card-head-icon" aria-hidden="true" />
            channels + workflows
          </div>

          <div className="card-body">
            <p className="card-desc">
              Send a message — a workflow sleeps 3 seconds, then broadcasts it to everyone on this
              page.
            </p>
            <form
              className="input-row"
              onSubmit={(e) => {
                e.preventDefault();
                void handleSend();
              }}
            >
              <input
                ref={inputRef}
                className="msg-input"
                type="text"
                placeholder="Type a message…"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                disabled={sending}
                autoFocus
              />
              <button
                type="submit"
                className="send-btn"
                disabled={sending || !input.trim()}
                aria-label="Send"
              >
                <Send size={16} aria-hidden="true" />
              </button>
            </form>
            {pending.size > 0 && (
              <p className="sending-hint">
                {pending.size === 1
                  ? "workflow running — arriving in ~3s…"
                  : `${pending.size} workflows running — arriving in ~3s…`}
              </p>
            )}
            {error && <p className="send-error">send failed: {error}</p>}
          </div>

          {messages.length > 0 && (
            <div className="messages" role="log" aria-live="polite">
              {messages.map((msg) => (
                <div key={msg.id} className="message">
                  <MessageCircle size={14} className="message-icon" aria-hidden="true" />
                  <span className="message-text">{msg.text}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      </main>

      <footer className="page-footer">
        <div className="footer-inner">
          <a href="https://tako.sh" target="_blank" rel="noopener noreferrer">
            tako.sh
          </a>
          <a href="https://tako.sh/docs" target="_blank" rel="noopener noreferrer">
            docs
          </a>
          <a href="https://github.com/lilienblum/tako" target="_blank" rel="noopener noreferrer">
            github
          </a>
        </div>
        <div className="footer-note">built with tako.sh</div>
      </footer>
    </div>
  );
}
