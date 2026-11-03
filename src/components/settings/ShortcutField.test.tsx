import { fireEvent, render, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { ShortcutField } from "./ShortcutField";
it("accepts Shift+Print Screen when Windows only sends keyup", () => {
  const change = vi.fn();
  render(<ShortcutField value="Ctrl+Shift+Digit2" active onChange={change} />);
  fireEvent.click(screen.getByRole("button"));
  fireEvent.keyUp(window, { key: "PrintScreen", code: "PrintScreen", shiftKey: true });
  expect(change).toHaveBeenCalledExactlyOnceWith("Shift+PrintScreen");
});
it("does not save a modifier on its own", () => {
  const change = vi.fn();
  render(<ShortcutField value="Ctrl+Shift+Digit2" active onChange={change} />);
  fireEvent.click(screen.getByRole("button"));
  fireEvent.keyDown(window, { key: "Shift", code: "ShiftLeft", shiftKey: true });
  expect(change).not.toHaveBeenCalled();
  fireEvent.keyDown(window, { key: "Escape", code: "Escape" });
  expect(screen.queryByText("Press the keys…")).toBeNull();
});
