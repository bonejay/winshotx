/**
 * La barra que acompanna a la grabacion.
 *
 * Lo que se comprueba aqui es lo que un fallo dejaria pasar sin que nadie lo viera hasta
 * perder una grabacion: que descartar pide dos toques, que «Guardando…» no vuelve atras,
 * y que la barra dice que se esta grabando sin tener que preguntar nada.
 */
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CACHE_AVISO, DESCARTAR_ARMADO_MS, RecorderBar } from "./RecorderBar";
import { aplicarIdioma } from "../../lib/i18n";
import { EVENTS, type RecordingTick } from "../../lib/types";
import { emite, llamadas } from "../../test/preparar";

function tick(cambios: Partial<RecordingTick> = {}): RecordingTick {
  return {
    elapsedMs: 12_000,
    frames: 360,
    bytes: 20_000_000,
    paused: false,
    saving: false,
    format: "video",
    audio: false,
    microphone: false,
    ...cambios,
  };
}

const comandos = () => llamadas.map((l) => l.comando);

beforeEach(() => {
  aplicarIdioma("es");
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("lo que dice la barra", () => {
  it("ensenna el tiempo que manda Rust y el formato", async () => {
    render(<RecorderBar />);
    await act(async () => {
      emite(EVENTS.recordingTick, tick({ elapsedMs: 83_000, format: "gif" }));
    });
    expect(screen.getByRole("timer")).toHaveTextContent("1:23");
    expect(screen.getByText("GIF")).toBeInTheDocument();
    expect(screen.queryByText("MP4")).toBeNull();
  });

  it("solo ensenna el sonido que entra de verdad", async () => {
    render(<RecorderBar />);
    await act(async () => {
      emite(EVENTS.recordingTick, tick({ audio: true, microphone: false }));
    });
    expect(screen.getByLabelText("System sound being recorded")).toBeInTheDocument();
  });

  it("el tamanno solo aparece cuando el cache ya pesa de verdad", async () => {
    render(<RecorderBar />);
    await act(async () => {
      emite(EVENTS.recordingTick, tick({ bytes: 20_000_000 }));
    });
    expect(screen.queryByText(/MB/)).toBeNull();
    await act(async () => {
      emite(EVENTS.recordingTick, tick({ bytes: CACHE_AVISO + 1 }));
    });
    expect(screen.getByText(/GB|MB/)).toBeInTheDocument();
  });

  it("en pausa lo dice, y el boton pasa a reanudar", async () => {
    render(<RecorderBar />);
    await act(async () => {
      emite(EVENTS.recordingTick, tick({ paused: true }));
    });
    expect(screen.getByText("paused")).toBeInTheDocument();
    expect(screen.getByLabelText("Resume")).toBeInTheDocument();
  });

  it("en ingles no queda ni un texto en espannol", async () => {
    aplicarIdioma("en");
    render(<RecorderBar />);
    await act(async () => {
      emite(EVENTS.recordingTick, tick({ audio: true, microphone: true, paused: false }));
    });
    expect(screen.getByLabelText("Pause")).toBeInTheDocument();
    expect(screen.getByLabelText("Stop")).toBeInTheDocument();
    expect(screen.getByLabelText("Discard")).toBeInTheDocument();
    expect(screen.getByLabelText("System sound being recorded")).toBeInTheDocument();
    expect(screen.getByLabelText("Microphone being recorded")).toBeInTheDocument();
  });
});

describe("descartar pide dos toques", () => {
  it("un clic solo arma el boton, y el segundo es el que tira la grabacion", () => {
    render(<RecorderBar />);
    fireEvent.click(screen.getByLabelText("Discard"));
    expect(comandos()).not.toContain("cancel_recording");
    expect(screen.getByLabelText("Discard?")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("Discard?"));
    expect(comandos()).toContain("cancel_recording");
  });

  it("si nadie confirma, el boton se desarma solo", () => {
    render(<RecorderBar />);
    fireEvent.click(screen.getByLabelText("Discard"));
    act(() => {
      vi.advanceTimersByTime(DESCARTAR_ARMADO_MS + 10);
    });
    expect(screen.getByLabelText("Discard")).toBeInTheDocument();
    expect(comandos()).not.toContain("cancel_recording");
  });

  it("Escape hace lo mismo: uno arma, dos descartan", () => {
    render(<RecorderBar />);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(comandos()).not.toContain("cancel_recording");
    fireEvent.keyDown(window, { key: "Escape" });
    expect(comandos()).toContain("cancel_recording");
  });
});

describe("parar", () => {
  it("pide parar una sola vez y se queda en Guardando con los botones apagados", () => {
    render(<RecorderBar />);
    fireEvent.click(screen.getByLabelText("Stop"));
    fireEvent.click(screen.getByLabelText("Stop"));
    expect(comandos().filter((c) => c === "stop_recording")).toHaveLength(1);
    expect(screen.getByText("Saving…")).toBeInTheDocument();
    expect(screen.getByLabelText("Stop")).toBeDisabled();
    expect(screen.getByLabelText("Pause")).toBeDisabled();
    expect(screen.getByLabelText("Discard")).toBeDisabled();
  });

  it("Guardando no vuelve atras aunque llegue un tick viejo detras", async () => {
    render(<RecorderBar />);
    await act(async () => {
      emite(EVENTS.recordingTick, tick({ saving: true }));
    });
    expect(screen.getByText("Saving…")).toBeInTheDocument();
    await act(async () => {
      emite(EVENTS.recordingTick, tick({ saving: false }));
    });
    expect(screen.getByText("Saving…")).toBeInTheDocument();
    expect(screen.getByLabelText("Stop")).toBeDisabled();
  });
});

describe("pausar", () => {
  it("pinta la pausa al pulsar, sin esperar al siguiente tick", () => {
    render(<RecorderBar />);
    fireEvent.click(screen.getByLabelText("Pause"));
    expect(llamadas.find((l) => l.comando === "pause_recording")?.args).toEqual({ paused: true });
    expect(screen.getByLabelText("Resume")).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText("Resume"));
    expect(llamadas.filter((l) => l.comando === "pause_recording").at(-1)?.args).toEqual({
      paused: false,
    });
  });

  it("la barra espaciadora pausa y reanuda", () => {
    render(<RecorderBar />);
    fireEvent.keyDown(document.body, { key: " " });
    expect(llamadas.find((l) => l.comando === "pause_recording")?.args).toEqual({ paused: true });
  });
});
