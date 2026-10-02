import { createContext } from "react";

/**
 * Where the enclosing tile sits, as a string that changes whenever its place does. A tile can move
 * without changing size — a column to its left closing, say — and a resize observer never hears
 * of that, so a pane that publishes its rectangle to the host measures again when this changes.
 */
export const PaneTileGeometryContext = createContext("");
