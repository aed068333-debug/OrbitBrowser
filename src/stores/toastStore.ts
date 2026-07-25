import { create } from "zustand";

export type ToastTone = "info" | "success" | "error";

export interface ToastItem {
  id: string;
  message: string;
  tone: ToastTone;
  createdAt: number;
  durationMs: number;
  /** slight horizontal offset so stacked toasts don't feel glued to one fixed slot */
  offsetX: number;
}

interface ToastState {
  items: ToastItem[];
  push: (message: string, tone?: ToastTone, durationMs?: number) => string;
  dismiss: (id: string) => void;
  clear: () => void;
}

function createId() {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `toast-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

export const useToastStore = create<ToastState>((set) => ({
  items: [],
  push: (message, tone = "info", durationMs = 3200) => {
    const id = createId();
    const offsetX = Math.round((Math.random() * 28 - 14) * 10) / 10;
    set((state) => ({
      items: [
        {
          id,
          message,
          tone,
          createdAt: Date.now(),
          durationMs,
          offsetX,
        },
        ...state.items,
      ].slice(0, 5),
    }));
    return id;
  },
  dismiss: (id) =>
    set((state) => ({
      items: state.items.filter((item) => item.id !== id),
    })),
  clear: () => set({ items: [] }),
}));

export function toast(
  message: string,
  tone: ToastTone = "info",
  durationMs = 3200,
) {
  return useToastStore.getState().push(message, tone, durationMs);
}

export function toastSuccess(message: string, durationMs = 2800) {
  return toast(message, "success", durationMs);
}

export function toastError(message: string, durationMs = 4200) {
  return toast(message, "error", durationMs);
}
