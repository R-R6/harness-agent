import { useCallback, useEffect, useRef, useState } from "react";

export type SplitOrientation = "vertical" | "horizontal";

interface Props {
  orientation: SplitOrientation;
  label: string;
  value: number;
  min: number | (() => number);
  max: number | (() => number);
  onChange: (value: number) => void;
  pixelsPerUnit?: number;
  step?: number;
  className?: string;
  valueText?: string;
}

interface ActiveDrag {
  pointerId: number;
  startCoordinate: number;
  startValue: number;
  previousCursor: string;
  previousUserSelect: string;
}

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), Math.max(min, max));
}

function resolveValue(value: number | (() => number)) {
  return typeof value === "function" ? value() : value;
}

export function SplitHandle({
  orientation,
  label,
  value,
  min,
  max,
  onChange,
  pixelsPerUnit = 1,
  step = 24,
  className = "",
  valueText,
}: Props) {
  const [dragging, setDragging] = useState(false);
  const dragRef = useRef<ActiveDrag | null>(null);
  const safePixelsPerUnit = pixelsPerUnit > 0 ? pixelsPerUnit : 1;
  const cursor = orientation === "vertical" ? "col-resize" : "row-resize";
  const resolvedMin = resolveValue(min);
  const resolvedMax = resolveValue(max);

  const restoreDocumentState = useCallback(() => {
    const drag = dragRef.current;
    if (!drag) return;
    document.body.style.cursor = drag.previousCursor;
    document.body.style.userSelect = drag.previousUserSelect;
    delete document.documentElement.dataset.splitDragging;
    dragRef.current = null;
  }, []);

  const finishDrag = useCallback((event?: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag || (event && event.pointerId !== drag.pointerId)) return;
    if (event?.currentTarget.hasPointerCapture?.(drag.pointerId)) {
      event.currentTarget.releasePointerCapture(drag.pointerId);
    }
    restoreDocumentState();
    setDragging(false);
  }, [restoreDocumentState]);

  useEffect(() => () => restoreDocumentState(), [restoreDocumentState]);

  const handlePointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    restoreDocumentState();
    dragRef.current = {
      pointerId: event.pointerId,
      startCoordinate: orientation === "vertical" ? event.clientX : event.clientY,
      startValue: value,
      previousCursor: document.body.style.cursor,
      previousUserSelect: document.body.style.userSelect,
    };
    document.body.style.cursor = cursor;
    document.body.style.userSelect = "none";
    document.documentElement.dataset.splitDragging = "true";
    event.currentTarget.setPointerCapture?.(event.pointerId);
    setDragging(true);
  };

  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    const coordinate = orientation === "vertical" ? event.clientX : event.clientY;
    onChange(clamp(
      drag.startValue + (coordinate - drag.startCoordinate) / safePixelsPerUnit,
      resolveValue(min),
      resolveValue(max),
    ));
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    const currentMin = resolveValue(min);
    const currentMax = resolveValue(max);
    const decreaseKey = orientation === "vertical" ? "ArrowLeft" : "ArrowUp";
    const increaseKey = orientation === "vertical" ? "ArrowRight" : "ArrowDown";
    let nextValue: number | null = null;
    if (event.key === decreaseKey) nextValue = value - step;
    if (event.key === increaseKey) nextValue = value + step;
    if (event.key === "Home") nextValue = currentMin;
    if (event.key === "End") nextValue = currentMax;
    if (nextValue === null) return;
    event.preventDefault();
    onChange(clamp(nextValue, currentMin, currentMax));
  };

  return (
    <div
      className={`split-handle split-handle--${orientation} ${dragging ? "split-handle--dragging" : ""} ${className}`.trim()}
      role="separator"
      tabIndex={0}
      aria-label={label}
      aria-orientation={orientation}
      aria-valuemin={Math.round(resolvedMin)}
      aria-valuemax={Math.round(Math.max(resolvedMin, resolvedMax))}
      aria-valuenow={Math.round(clamp(value, resolvedMin, resolvedMax))}
      aria-valuetext={valueText ?? `${Math.round(value)} 像素`}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={finishDrag}
      onPointerCancel={finishDrag}
      onKeyDown={handleKeyDown}
    />
  );
}
