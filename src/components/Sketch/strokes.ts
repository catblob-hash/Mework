export type SketchToolName = "pen" | "line" | "arrow" | "rect" | "ellipse" | "text";
export type SketchShapeKind = "line" | "arrow" | "rect" | "ellipse";

export interface SketchPoint {
  x: number;
  y: number;
}

export interface SketchFreehandStroke {
  kind: "freehand";
  points: SketchPoint[];
  color: string;
  width: number;
}

export interface SketchShapeStroke {
  kind: SketchShapeKind;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  color: string;
  width: number;
}

export interface SketchTextStroke {
  kind: "text";
  x: number;
  y: number;
  text: string;
  size: number;
  color: string;
}

export type SketchStroke = SketchFreehandStroke | SketchShapeStroke | SketchTextStroke;
export type SketchStrokeKind = SketchStroke["kind"];

/** Palette order. The first entry is the tool a fresh surface starts on. */
export const SKETCH_TOOLS: readonly SketchToolName[] = ["pen", "line", "arrow", "rect", "ellipse", "text"];

/** Red, blue, green, black. There is no custom colour picker. */
export const SKETCH_COLORS: readonly string[] = ["#E03131", "#1971C2", "#2F9E44", "#1F1E1D"];

export const SKETCH_STROKE_WIDTH = 4;
export const SKETCH_TEXT_SIZE = 16;

export const SKETCH_FONT_STACK = '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif';

const SNAP_ANGLE = Math.PI / 4;

/** Shift-constrain: 45° increments for line and arrow, a square for rect and ellipse. */
export function constrainPoint(
  kind: SketchStrokeKind,
  origin: SketchPoint,
  point: SketchPoint,
  constrained: boolean
): SketchPoint {
  if (!constrained) return point;
  const dx = point.x - origin.x;
  const dy = point.y - origin.y;
  if (kind === "line" || kind === "arrow") {
    const length = Math.hypot(dx, dy);
    const angle = Math.round(Math.atan2(dy, dx) / SNAP_ANGLE) * SNAP_ANGLE;
    return { x: origin.x + length * Math.cos(angle), y: origin.y + length * Math.sin(angle) };
  }
  if (kind === "rect" || kind === "ellipse") {
    const side = Math.max(Math.abs(dx), Math.abs(dy));
    return { x: origin.x + side * (Math.sign(dx) || 1), y: origin.y + side * (Math.sign(dy) || 1) };
  }
  return point;
}

export function drawStroke(context: CanvasRenderingContext2D, stroke: SketchStroke): void {
  context.lineCap = "round";
  context.lineJoin = "round";
  context.strokeStyle = stroke.color;
  context.fillStyle = stroke.color;

  if (stroke.kind === "freehand") {
    const points = stroke.points;
    if (points.length === 0) return;
    context.lineWidth = stroke.width;
    if (points.length === 1) {
      context.beginPath();
      context.arc(points[0]!.x, points[0]!.y, stroke.width / 2, 0, Math.PI * 2);
      context.fill();
      return;
    }
    context.beginPath();
    context.moveTo(points[0]!.x, points[0]!.y);
    for (let index = 1; index < points.length; index += 1) context.lineTo(points[index]!.x, points[index]!.y);
    context.stroke();
    return;
  }

  if (stroke.kind === "line") {
    context.lineWidth = stroke.width;
    context.beginPath();
    context.moveTo(stroke.x1, stroke.y1);
    context.lineTo(stroke.x2, stroke.y2);
    context.stroke();
    return;
  }

  if (stroke.kind === "rect") {
    context.lineWidth = stroke.width;
    context.strokeRect(
      Math.min(stroke.x1, stroke.x2),
      Math.min(stroke.y1, stroke.y2),
      Math.abs(stroke.x2 - stroke.x1),
      Math.abs(stroke.y2 - stroke.y1)
    );
    return;
  }

  if (stroke.kind === "arrow") {
    const dx = stroke.x2 - stroke.x1;
    const dy = stroke.y2 - stroke.y1;
    const length = Math.hypot(dx, dy) || 1;
    const unitX = dx / length;
    const unitY = dy / length;
    const head = Math.min(stroke.width * 4, length);
    const shaftCut = Math.min(head * 0.7, length);
    context.lineWidth = stroke.width;
    context.beginPath();
    context.moveTo(stroke.x1, stroke.y1);
    context.lineTo(stroke.x2 - shaftCut * unitX, stroke.y2 - shaftCut * unitY);
    context.stroke();
    context.beginPath();
    context.moveTo(stroke.x2, stroke.y2);
    context.lineTo(stroke.x2 - head * unitX + (head / 2) * unitY, stroke.y2 - head * unitY - (head / 2) * unitX);
    context.lineTo(stroke.x2 - head * unitX - (head / 2) * unitY, stroke.y2 - head * unitY + (head / 2) * unitX);
    context.closePath();
    context.fill();
    return;
  }

  if (stroke.kind === "ellipse") {
    context.lineWidth = stroke.width;
    context.beginPath();
    context.ellipse(
      (stroke.x1 + stroke.x2) / 2,
      (stroke.y1 + stroke.y2) / 2,
      Math.abs(stroke.x2 - stroke.x1) / 2,
      Math.abs(stroke.y2 - stroke.y1) / 2,
      0,
      0,
      Math.PI * 2
    );
    context.stroke();
    return;
  }

  if (stroke.kind !== "text") return;
  context.font = `${stroke.size}px ${SKETCH_FONT_STACK}`;
  context.textBaseline = "top";
  const lines = stroke.text.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    context.fillText(lines[index]!, stroke.x, stroke.y + index * stroke.size * 1.2);
  }
}
