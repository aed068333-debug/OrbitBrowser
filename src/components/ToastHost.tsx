import { AlertCircle, CheckCircle2, Info, X } from "lucide-react";
import { useEffect } from "react";

import { useToastStore, type ToastItem } from "@/stores/toastStore";

function toneClass(tone: ToastItem["tone"]) {
  switch (tone) {
    case "success":
      return "border-ok/25 bg-white text-ok shadow-lg";
    case "error":
      return "border-danger/25 bg-white text-danger shadow-lg";
    default:
      return "border-line bg-white text-ink-800 shadow-lg";
  }
}

function ToneIcon({ tone }: { tone: ToastItem["tone"] }) {
  if (tone === "success") {
    return <CheckCircle2 className="h-4 w-4 shrink-0 text-ok" />;
  }
  if (tone === "error") {
    return <AlertCircle className="h-4 w-4 shrink-0 text-danger" />;
  }
  return <Info className="h-4 w-4 shrink-0 text-brand-600" />;
}

export function ToastHost() {
  const items = useToastStore((state) => state.items);
  const dismiss = useToastStore((state) => state.dismiss);

  useEffect(() => {
    if (items.length === 0) {
      return;
    }
    const timers = items.map((item) =>
      window.setTimeout(() => dismiss(item.id), item.durationMs),
    );
    return () => {
      for (const timer of timers) {
        window.clearTimeout(timer);
      }
    };
  }, [dismiss, items]);

  if (items.length === 0) {
    return null;
  }

  return (
    <div
      aria-live="polite"
      className="pointer-events-none fixed inset-x-0 top-4 z-[80] flex flex-col items-end gap-2 px-4 sm:top-5 sm:px-6"
    >
      {items.map((item, index) => (
        <div
          className={`pointer-events-auto flex w-full max-w-sm items-start gap-2.5 rounded-xl border px-3.5 py-3 text-sm leading-5 transition duration-200 ${toneClass(item.tone)}`}
          key={item.id}
          style={{
            transform: `translateX(${item.offsetX}px)`,
            marginRight: `${(index % 3) * 6}px`,
          }}
        >
          <ToneIcon tone={item.tone} />
          <div className="min-w-0 flex-1 break-words text-ink-800">
            {item.message}
          </div>
          <button
            aria-label="Dismiss"
            className="control-focus -mr-1 -mt-0.5 rounded-md p-1 text-ink-400 transition hover:bg-ink-50 hover:text-ink-700"
            onClick={() => dismiss(item.id)}
            type="button"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      ))}
    </div>
  );
}
