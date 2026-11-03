import { AppWindow, Camera, RectangleHorizontal, Monitor, PanelBottom, Settings, Video, X } from "lucide-react";
import type { CaptureMode } from "../../lib/types";
import { useT } from "../../lib/i18n";

/**
 * Como reconoce el lienzo un gesto que nace en la barra.
 *
 * La barra ya no se queda los `pointerdown`: se puede empezar a recortar por debajo de
 * ella. El lienzo necesita distinguir ese gesto para no tratar el clic de un boton como
 * un clic suyo, y esta constante es la unica atadura entre los dos lados.
 */
export const SELECTOR_BARRA = "[data-barra-modos]";

interface Props {
  target?: import("../../lib/types").CaptureTarget;
  onTarget?: (v: import("../../lib/types").CaptureTarget) => void;
  value: CaptureMode;
  onChange: (mode: CaptureMode) => void;
  /** Coger la pantalla entera de un clic, sin arrastrar nada. */
  pantallaEntera: boolean;
  onPantallaEntera: (valor: boolean) => void;
  /** Si al soltar el recorte sale la barra para elegir qué hacer con él. */
  conBarra: boolean;
  onConBarra: (valor: boolean) => void;
  /** Abre los ajustes de winshotx, cerrando la captura. */
  onAjustes: () => void;
  onCancel: () => void;
  /** Se aparta mientras se arrastra, para no tapar lo que se está recortando. */
  dimmed: boolean;
}

/**
 * Qué se va a hacer con el recorte, elegido ANTES de recortar.
 *
 * Va arriba y centrada, donde Windows pone la suya, porque es el sitio donde la gente
 * ya la busca. Y hace falta: en el perfil "se copia sola" no salía ninguna barra, así
 * que desde ese perfil no había forma de grabar nada.
 *
 * Solo iconos, sin etiqueta: una cámara, una cámara de vídeo y las letras GIF se leen de
 * un vistazo, y tres palabras al lado hacían la barra el doble de ancha para no decir
 * nada más. El texto vive en el tooltip, para quien lo necesite.
 */
export function ModeBar({
  target = "rectangle",
  onTarget,
  value,
  onChange,
  pantallaEntera,
  onPantallaEntera,
  conBarra,
  onConBarra,
  onAjustes,
  onCancel,
  dimmed,
}: Props) {
  const t = useT();
  return (
    <div
      data-barra-modos
      className={`group absolute top-6 left-1/2 z-50 -translate-x-1/2 transition-opacity duration-150 ${
        dimmed ? "pointer-events-none opacity-0" : "pointer-events-auto opacity-100"
      }`}
    >
      {/* Al pasar el raton por encima se aparta: pierde el panel, la sombra y el
          desenfoque, y se queda en un fantasma. La franja de arriba del centro es sitio
          de capturar, y un rectangulo negro ahi tapaba justo lo que se iba a recortar.
          Sigue pulsandose igual, y el arrastre la atraviesa (ver SELECTOR_BARRA). */}
      <div className="flex items-center gap-1 rounded-2xl border border-white/10 bg-neutral-900/90 p-1.5 shadow-2xl backdrop-blur-xl transition-[background-color,border-color,box-shadow,backdrop-filter] duration-150 ">
        <Boton
          activo={value === "still"}
          onClick={() => onChange("still")}
          etiqueta="Photo"
          titulo="Photo of the crop · F"
        >
          <Camera className="size-[19px]" />
        </Boton>

        <Boton
          activo={value === "video"}
          onClick={() => onChange("video")}
          etiqueta="Video"
          titulo="Record the crop to MP4 · V"
        >
          <Video className="size-[19px]" />
        </Boton>

        <span className="mx-1 h-6 w-px bg-white/10" />

        {/* El otro eje: los tres de arriba dicen QUE sale, este dice DE DONDE. Con varias
            pantallas, cada una se pone su numero encima y se coge con un clic. */}
        <Boton activo={!pantallaEntera && target === "rectangle"} onClick={() => onTarget?.("rectangle")} etiqueta="Rectangle" titulo="Select a rectangle"><RectangleHorizontal className="size-[19px]" /></Boton>
        <Boton activo={!pantallaEntera && target === "window"} onClick={() => onTarget?.("window")} etiqueta="Window" titulo="Select a window"><AppWindow className="size-[19px]" /></Boton>
        <Boton
          activo={pantallaEntera}
          onClick={() => onTarget ? onTarget(pantallaEntera ? "rectangle" : "screen") : onPantallaEntera(!pantallaEntera)}
          etiqueta="Whole screen"
          titulo="Whole screen, one click · P"
        >
          <Monitor className="size-[19px]" />
        </Boton>

        <span className="mx-1 h-6 w-px bg-white/10" />

        {/* Y el tercer eje: que pasa DESPUES de soltar. Encendido sale la barra de la
            seleccion (copiar, guardar, editar, anclar, texto); apagado la foto se copia
            sola y la captura se cierra. Estaba solo en los ajustes, dos ventanas mas
            alla, cuando es una decision que se cambia captura a captura. Grabando no
            pinta nada: en video y en GIF la barra siempre sale, porque es donde se
            ajusta el recuadro antes de empezar. */}
        {value === "still" && (
          <Boton
            activo={conBarra}
            onClick={() => onConBarra(!conBarra)}
            etiqueta="Choose what to do"
            titulo="On release, choose what to do with the crop · B"
          >
            <PanelBottom className="size-[19px]" />
          </Boton>
        )}

        {/* Sin `aria-pressed`: esto no se queda pulsado, abre una ventana. Va aqui
            porque desde una captura a pantalla completa no hay forma de llegar a los
            ajustes sin cerrarla primero y buscar el icono de la bandeja. */}
        <button
          type="button"
          onClick={onAjustes}
          title={t("winshotx settings")}
          aria-label={t("Settings")}
          className="flex size-9 items-center justify-center rounded-xl text-neutral-300 transition-colors hover:bg-white/10 hover:text-white"
        >
          <Settings className="size-[19px]" />
        </button>

        <span className="mx-1 h-6 w-px bg-white/10" />

        <button
          type="button"
          onClick={onCancel}
          title={t("Leave without capturing · Esc")}
          aria-label={t("Leave without capturing")}
          className="flex size-9 items-center justify-center rounded-xl text-neutral-400 transition-colors hover:bg-red-500/20 hover:text-red-300"
        >
          <X className="size-[19px]" />
        </button>
      </div>
    </div>
  );
}

/**
 * Las letras GIF dentro de su caja, al peso de los iconos de lucide que tiene al lado.
 *
 * No hay dibujo que signifique "GIF": el que había antes eran unas chispas, que no lo
 * decían. Las letras van como trazos, no como texto, para que engorden con el mismo
 * `stroke-width` que la cámara y no salgan finas a su lado.
 */

function Boton({
  activo,
  onClick,
  etiqueta,
  titulo,
  children,
}: {
  activo: boolean;
  onClick: () => void;
  etiqueta: string;
  titulo: string;
  children: React.ReactNode;
}) {
  const t = useT();
  return (
    <button
      type="button"
      onClick={onClick}
      title={t(titulo)}
      aria-label={t(etiqueta)}
      aria-pressed={activo}
      className={`flex size-9 items-center justify-center rounded-xl transition-colors ${
        activo
          ? "bg-blue-500 text-white shadow-sm"
          : "text-neutral-300 hover:bg-white/10 hover:text-white"
      }`}
    >
      {children}
    </button>
  );
}
