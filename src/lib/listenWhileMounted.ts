import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * `listen()` is async. React StrictMode (and fast remounts) run cleanup before
 * the unlisten handle exists, which would leak a second subscriber and paint
 * every chunk twice — 组件卸载早于订阅完成时，迟到的订阅必须立即注销。
 */
export function listenWhileMounted<T>(
  event: string,
  handler: (event: { payload: T }) => void,
): () => void {
  let cancelled = false;
  let unlisten: UnlistenFn | undefined;
  void listen<T>(event, handler).then(
    (fn) => {
      if (cancelled) {
        fn();
        return;
      }
      unlisten = fn;
    },
    () => {
      // Subscribe failed; the next mount retries.
    },
  );
  return () => {
    cancelled = true;
    unlisten?.();
  };
}
