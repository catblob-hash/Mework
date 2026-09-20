export const CSP_STYLE_NONCE_CARRIER_ID = "mework-csp-style-nonce";

interface StyleNonceInstallation {
  nonce: string;
  references: number;
  originalDescriptor: PropertyDescriptor | undefined;
  createElementWithNonce: Document["createElement"];
}

const styleNonceInstallations = new WeakMap<Document, StyleNonceInstallation>();

export function requireCspStyleNonce(document: Document): string {
  const carrier = document.getElementById(CSP_STYLE_NONCE_CARRIER_ID);
  if (carrier?.localName !== "style") {
    throw new Error(`缺少前端 CSP 样式 nonce 载体 #${CSP_STYLE_NONCE_CARRIER_ID}`);
  }
  const nonce = (carrier as HTMLStyleElement).nonce.trim();
  if (!nonce) {
    throw new Error(`前端 CSP 样式 nonce 载体 #${CSP_STYLE_NONCE_CARRIER_ID} 未被宿主 nonce 化`);
  }
  return nonce;
}

/**
 * Keep dynamic styles nonce-safe for the lifetime of a mounted xterm instance.
 * xterm creates some styles during open(), then creates or replaces others only
 * after its first write, fit, renderer activation, or theme update.
 */
export function installCspStyleNonce(document: Document): () => void {
  const nonce = requireCspStyleNonce(document);
  const existing = styleNonceInstallations.get(document);
  if (existing) {
    if (document.createElement !== existing.createElementWithNonce) {
      throw new Error("前端 CSP 样式 nonce 保护已被外部覆盖");
    }
    if (existing.nonce !== nonce) {
      throw new Error("前端 CSP 样式 nonce 在同一文档生命周期内发生变化");
    }
    existing.references += 1;
    return releaseInstallation(document, existing);
  }

  const originalDescriptor = Object.getOwnPropertyDescriptor(document, "createElement");
  const originalCreateElement = document.createElement;
  const createElementWithNonce = function (
    this: Document,
    qualifiedName: string,
    options?: ElementCreationOptions
  ): Element {
    const args = options === undefined ? [qualifiedName] : [qualifiedName, options];
    const element = Reflect.apply(originalCreateElement, this, args) as Element;
    if (qualifiedName.toLowerCase() === "style") {
      (element as HTMLStyleElement).nonce = nonce;
    }
    return element;
  } as unknown as Document["createElement"];

  Object.defineProperty(document, "createElement", {
    configurable: true,
    enumerable: originalDescriptor?.enumerable ?? false,
    writable: true,
    value: createElementWithNonce
  });

  const installation: StyleNonceInstallation = {
    nonce,
    references: 1,
    originalDescriptor,
    createElementWithNonce
  };
  styleNonceInstallations.set(document, installation);
  return releaseInstallation(document, installation);
}

function releaseInstallation(
  document: Document,
  installation: StyleNonceInstallation
): () => void {
  let released = false;
  return () => {
    if (released) return;
    released = true;

    const active = styleNonceInstallations.get(document);
    if (active !== installation) return;
    active.references -= 1;
    if (active.references > 0) return;

    styleNonceInstallations.delete(document);
    if (document.createElement !== active.createElementWithNonce) return;
    if (active.originalDescriptor) {
      Object.defineProperty(document, "createElement", active.originalDescriptor);
    } else {
      Reflect.deleteProperty(document, "createElement");
    }
  };
}
