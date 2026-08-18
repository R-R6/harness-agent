import {
  useEffect,
  useLayoutEffect,
  useState,
  type Dispatch,
  type RefObject,
  type SetStateAction,
} from "react";

export function useStoredNumber(key: string, fallback: number): [number, Dispatch<SetStateAction<number>>] {
  const [value, setValue] = useState(() => {
    try {
      const saved = Number(localStorage.getItem(key));
      return Number.isFinite(saved) && saved > 0 ? saved : fallback;
    } catch {
      return fallback;
    }
  });

  useEffect(() => {
    try {
      localStorage.setItem(key, String(value));
    } catch {
      // Layout persistence is optional; resizing remains available in memory.
    }
  }, [key, value]);

  return [value, setValue];
}

export function useMediaQuery(query: string) {
  const getMatches = () => typeof window.matchMedia === "function" && window.matchMedia(query).matches;
  const [matches, setMatches] = useState(getMatches);

  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const media = window.matchMedia(query);
    const update = () => setMatches(media.matches);
    update();
    media.addEventListener?.("change", update);
    return () => media.removeEventListener?.("change", update);
  }, [query]);

  return matches;
}

export function useElementSize(ref: RefObject<HTMLElement | null>) {
  const [size, setSize] = useState({ width: 0, height: 0 });

  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const measure = () => {
      const rect = element.getBoundingClientRect();
      const width = rect.width || element.clientWidth;
      const height = rect.height || element.clientHeight;
      setSize((current) => current.width === width && current.height === height
        ? current
        : { width, height });
    };

    measure();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", measure);
      return () => window.removeEventListener("resize", measure);
    }
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref]);

  return size;
}
