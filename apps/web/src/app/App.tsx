import { Route, Routes } from "react-router";
import { COMPANY_NAME, PRODUCT_NAME, PRODUCT_TAGLINE } from "@aushadharth/configuration";
import { ServiceStatus } from "../components/ServiceStatus";

function FoundationShell() {
  return (
    <main className="shell">
      <section className="brand" aria-labelledby="product-name">
        <p className="eyebrow">{COMPANY_NAME}</p>
        <h1 id="product-name">{PRODUCT_NAME}</h1>
        <p className="tagline">{PRODUCT_TAGLINE}</p>
        <p className="company">A Product of {COMPANY_NAME}</p>
        <ServiceStatus />
      </section>
    </main>
  );
}

export function App() {
  return (
    <Routes>
      <Route path="*" element={<FoundationShell />} />
    </Routes>
  );
}
