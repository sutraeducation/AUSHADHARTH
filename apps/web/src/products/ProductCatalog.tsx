import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, Navigate, useNavigate, useParams } from "react-router";
import type {
  Barcode,
  CreateProductRequest,
  Product,
  ProductCompanyRole,
  ProductCompanyRoleFields,
  ProductDetail,
  ProductFields,
  ProductPack,
  ProductPackFields,
  ReferenceKind,
  ReferenceMasterResponse,
  StorePackPolicy,
  StorePackPolicyFields
} from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { LocalServiceError } from "../platform/localService";
import { listReferences } from "../reference/referenceApi";
import { CatalogDialog } from "./CatalogDialog";
import {
  atomsToQuantity,
  changeBarcodeLifecycle,
  changeCompanyRoleLifecycle,
  changePackPolicyLifecycle,
  changePackLifecycle,
  changeProductLifecycle,
  createBarcode,
  createCompanyRole,
  createPack,
  createProduct,
  findDuplicateCandidates,
  getCatalogContext,
  getPackPolicy,
  getProduct,
  listBarcodes,
  listProducts,
  quantityToAtoms,
  savePackPolicy,
  updateCompanyRole,
  updatePack,
  updateProduct,
  type CatalogStatusFilter
} from "./productApi";

type FieldErrors = Record<string, string>;
type ProductValues = {
  productKind: ProductFields["productKind"];
  brandId: string;
  dosageFormId: string;
  baseUnitId: string;
  quantityScale: string;
  displayName: string;
  formulationDescriptor: string;
  routeDescriptor: string;
  releaseDescriptor: string;
  initialPackUnitId: string;
  initialPackQuantity: string;
  initialPackLabel: string;
  initialSku: string;
  companyId: string;
  companyRole: ProductCompanyRoleFields["role"];
  barcodeNamespace: string;
  barcodeValue: string;
  configureDefaultPolicy: boolean;
};

const PRODUCT_KINDS = [
  ["medicine", "Medicine"],
  ["device", "Medical Device"],
  ["general_pharmacy_item", "General Pharmacy Item"]
] as const;

// Distinguishing pending from failed keeps a reference-label failure out of the success rendering.
type ReferenceState = "pending" | "error" | "ready";
type ReloadProduct = () => Promise<ProductDetail | null>;

const STALE_RECORD_MESSAGE = "This record was changed after you opened it. Reload the latest version before saving; nothing you entered has overwritten it.";

function isRevisionConflict(error: unknown) { return error instanceof LocalServiceError && error.code === "revision_conflict"; }

const ROLE_LABELS: Record<ProductCompanyRoleFields["role"], string> = {
  manufacturer: "Manufacturer",
  marketer: "Marketer",
  brand_owner: "Brand Owner",
  importer: "Importer"
};

export function ProductListPage() {
  usePageTitle("Products");
  const auth = useAuth();
  const [input, setInput] = useState("");
  const search = useDebouncedValue(input, 250);
  const [status, setStatus] = useState<CatalogStatusFilter>("active");
  const products = useQuery({ queryKey: ["products", search, status], queryFn: () => listProducts(search, status), retry: false });
  const references = useCatalogReferences();
  useExpireOnAuthError(products.error);
  const canMutate = auth.status?.user?.role === "owner_admin";
  return <>
    <PageHeading eyebrow="MASTERS" title="Products" description="Medicine & Pharmacy Product Catalog">
      {canMutate ? <Link className="button button--primary" to="/app/products/new">Add Product</Link> : <span className="read-only-note">Read-only access</span>}
    </PageHeading>
    <section className="master-panel" aria-labelledby="product-list-title">
      <div className="master-toolbar">
        <div className="search-field"><label htmlFor="product-search">Search</label><span><input id="product-search" value={input} onChange={(event) => setInput(event.target.value)} placeholder="Name, company, SKU, or barcode" />{input && <button type="button" onClick={() => setInput("")}>Clear</button>}</span></div>
        <div className="filter-field"><label htmlFor="product-status">Status</label><select id="product-status" value={status} onChange={(event) => setStatus(event.target.value as CatalogStatusFilter)}><option value="active">Active</option><option value="archived">Archived</option><option value="all">All</option></select></div>
      </div>
      <h2 id="product-list-title" className="sr-only">Product records</h2>
      {references.isError && <InlineQueryError label="Brand, Dosage Form, and Base Unit names could not be loaded." onRetry={() => void references.refetch()} />}
      {products.isPending ? <Loading label="Loading products…" /> : products.isError ? <QueryError label="Products could not be loaded" onRetry={() => void products.refetch()} /> : products.data.length === 0 ? <Empty search={Boolean(search)} archived={status === "archived"} /> :
        <div className="table-scroll"><table className="data-table"><thead><tr><th scope="col">Product</th><th scope="col">Kind</th><th scope="col">Brand</th><th scope="col">Dosage Form</th><th scope="col">Base Unit</th><th scope="col">Status</th><th scope="col"><span className="sr-only">Actions</span></th></tr></thead><tbody>{products.data.map((product) => <tr key={product.id}><td data-label="Product"><Link to={`/app/products/${product.id}`}>{product.displayName}</Link></td><td data-label="Kind">{kindLabel(product.productKind)}</td><td data-label="Brand">{referenceName(references.data?.brands, product.brandId, referenceState(references))}</td><td data-label="Dosage Form">{referenceName(references.data?.dosageForms, product.dosageFormId, referenceState(references))}</td><td data-label="Base Unit">{referenceName(references.data?.units, product.baseUnitId, referenceState(references))}</td><td data-label="Status"><Status value={product.status} /></td><td className="row-actions"><div><Link to={`/app/products/${product.id}`}>View</Link>{canMutate && product.status === "active" && <Link to={`/app/products/${product.id}/edit`}>Edit</Link>}</div></td></tr>)}</tbody></table></div>}
    </section>
  </>;
}

export function ProductCreatePage() {
  const auth = useAuth();
  if (auth.status?.user?.role !== "owner_admin") return <Navigate to="/app/products" replace />;
  return <ProductForm />;
}

export function ProductEditPage() {
  const { id } = useParams();
  const auth = useAuth();
  const product = useQuery({ queryKey: ["product", id], queryFn: () => getProduct(id!), enabled: Boolean(id), retry: false });
  useExpireOnAuthError(product.error);
  if (!id) return <Navigate to="/app/products" replace />;
  if (auth.status?.user?.role !== "owner_admin") return <Navigate to={`/app/products/${id}`} replace />;
  if (product.isPending) return <Loading label="Loading product…" />;
  if (product.isError) return <QueryError label="Product could not be loaded" onRetry={() => void product.refetch()} />;
  if (product.data.status !== "active") return <Navigate to={`/app/products/${id}`} replace />;
  return <ProductForm record={product.data} />;
}

function ProductForm({ record }: { record?: ProductDetail }) {
  usePageTitle(record ? `Edit ${record.displayName}` : "Add Product");
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const formRef = useRef<HTMLFormElement>(null);
  const references = useCatalogReferences();
  const context = useCatalogContext();
  const [values, setValues] = useState<ProductValues>(() => productValues(record));
  const [errors, setErrors] = useState<FieldErrors>({});
  const [notice, setNotice] = useState<string | null>(null);
  const [reloading, setReloading] = useState(false);
  const [candidates, setCandidates] = useState<Array<{ candidateId: string; score: number; reasonCodes: string[] }> | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const candidateNames = useQuery({ queryKey: ["products", "candidate-names"], queryFn: () => listProducts("", "all"), enabled: Boolean(candidates?.length), retry: false });
  const mutation = useMutation({
    mutationFn: async (request: CreateProductRequest | ProductFields) => record ? updateProduct(record, request as ProductFields) : createProduct(request as CreateProductRequest),
    onSuccess: (saved) => navigate(`/app/products/${saved.id}`),
    onError: (error) => {
      if (error instanceof LocalServiceError) { setNotice(error.message); setErrors(Object.fromEntries(error.issues.map((issue) => [issue.field, issue.message]))); }
      else setNotice("The Local Store Service could not save this product.");
    }
  });
  const set = <K extends keyof ProductValues>(field: K, value: ProductValues[K]) => { setValues((current) => ({ ...current, [field]: value })); setErrors((current) => { const next = { ...current }; delete next[field]; return next; }); setCandidates(null); setConfirmed(false); };
  const submit = async (event: FormEvent) => {
    event.preventDefault(); setNotice(null);
    const validation = validateProduct(values, Boolean(record)); setErrors(validation);
    if (Object.keys(validation).length) return;
    const product = productFields(values);
    if (record) { mutation.mutate(product); return; }
    if (!context.data) { setNotice(context.isError ? "The current Store could not be identified, so this Product cannot be created yet. Retry Store information and try again." : "Store information is still loading. Try again."); return; }
    const request = createRequest(values, product, context.data.storeId);
    if (!confirmed) {
      try {
        const found = await findDuplicateCandidates(request);
        if (found.length) { setCandidates(found); return; }
      } catch (error) { setNotice(error instanceof LocalServiceError ? error.message : "Possible duplicates could not be checked."); return; }
    }
    mutation.mutate(request);
  };
  useEffect(() => { const first = Object.keys(errors)[0]; if (first) formRef.current?.querySelector<HTMLElement>(`[name="${first}"]`)?.focus(); }, [Object.keys(errors).join("|")]);
  // A stale-revision save is never retried over newer data: the authoritative Product query is
  // invalidated, the refetch is awaited, and the editor is repopulated from the latest record.
  const reloadLatest = async () => {
    if (!record) return;
    setReloading(true);
    try {
      await queryClient.invalidateQueries({ queryKey: ["product", record.id] });
      const latest = queryClient.getQueryData<ProductDetail>(["product", record.id]);
      if (!latest) { setNotice("The latest version of this Product could not be loaded. Try again."); return; }
      setValues(productValues(latest)); setErrors({}); setNotice(null); setCandidates(null); setConfirmed(false); mutation.reset();
    } finally { setReloading(false); }
  };
  const scale = Number(values.quantityScale) || 0;
  const baseUnit = references.data?.units.find((unit) => unit.id === values.baseUnitId);
  const baseUnitIsDiscrete = (baseUnit?.attributes as Record<string, unknown> | undefined)?.isDiscrete === true;
  return <>
    <nav className="breadcrumbs" aria-label="Breadcrumb"><Link to="/app/products">Products</Link><span aria-hidden="true">/</span><span>{record ? "Edit" : "Add Product"}</span></nav>
    <PageHeading eyebrow="PRODUCT CATALOG" title={record ? `Edit ${record.displayName}` : "Add Product"} description={record ? `Revision ${record.revision}; stale changes are never overwritten.` : "Create the product identity and its first saleable presentation atomically."} />
    <form ref={formRef} className="catalog-form" onSubmit={(event) => void submit(event)} noValidate>
      <FormSection title="Product identity" description="Business identity only—no batch, stock, price, or composition data.">
        <SelectField label="Product kind" name="productKind" value={values.productKind} onChange={(value) => set("productKind", value as ProductValues["productKind"])} options={PRODUCT_KINDS} error={errors.productKind} />
        <TextField label="Display name" name="displayName" value={values.displayName} onChange={(value) => set("displayName", value)} required error={errors.displayName} />
        <ReferencePicker label="Brand" kind="brands" value={values.brandId} onChange={(value) => set("brandId", value)} optional />
        <ReferencePicker label="Dosage Form" kind="dosage-forms" value={values.dosageFormId} onChange={(value) => set("dosageFormId", value)} optional={values.productKind !== "medicine"} error={errors.dosageFormId} />
        <ReferencePicker label="Base Unit" kind="units" value={values.baseUnitId} onChange={(value, record) => { set("baseUnitId", value); const attrs = record?.attributes as Record<string, unknown> | undefined; if (attrs?.isDiscrete === true) set("quantityScale", "0"); }} error={errors.baseUnitId} />
        {baseUnitIsDiscrete ? <div className="field"><span className="field-label">Quantity precision</span><strong>Whole units only</strong><small>Discrete base units always use precision 0.</small><input type="hidden" name="quantityScale" value="0" /></div> : <TextField label="Quantity precision" name="quantityScale" type="number" value={values.quantityScale} onChange={(value) => set("quantityScale", value)} required error={errors.quantityScale} hint="Continuous units may use up to 6 decimal places." />}
      </FormSection>
      <FormSection title="Descriptors" description="Optional display descriptors; these are not clinical composition data.">
        <TextField label="Formulation descriptor" name="formulationDescriptor" value={values.formulationDescriptor} onChange={(value) => set("formulationDescriptor", value)} />
        <TextField label="Route descriptor" name="routeDescriptor" value={values.routeDescriptor} onChange={(value) => set("routeDescriptor", value)} />
        <TextField label="Release descriptor" name="releaseDescriptor" value={values.releaseDescriptor} onChange={(value) => set("releaseDescriptor", value)} />
      </FormSection>
      {!record && <FormSection title="Initial Pack / SKU" description="At least one Pack is created in the same atomic transaction.">
        <ReferencePicker label="Pack Unit" kind="units" value={values.initialPackUnitId} onChange={(value) => set("initialPackUnitId", value)} error={errors.initialPackUnitId} />
        <TextField label="Direct base quantity" name="initialPackQuantity" value={values.initialPackQuantity} onChange={(value) => set("initialPackQuantity", value)} required error={errors.initialPackQuantity} hint={`How many base units this Pack contains${scale ? `, with up to ${scale} decimals` : ""}.`} />
        <TextField label="Pack label" name="initialPackLabel" value={values.initialPackLabel} onChange={(value) => set("initialPackLabel", value)} hint="For example, Strip of 15." />
        <TextField label="SKU (optional)" name="initialSku" value={values.initialSku} onChange={(value) => set("initialSku", value)} />
        <ReferencePicker label="Company (optional)" kind="companies" value={values.companyId} onChange={(value) => set("companyId", value)} optional />
        {values.companyId && <SelectField label="Company role" name="companyRole" value={values.companyRole} onChange={(value) => set("companyRole", value as ProductValues["companyRole"])} options={Object.entries(ROLE_LABELS).map(([value, label]) => [value, label] as const)} />}
        <TextField label="Initial barcode (optional)" name="barcodeValue" value={values.barcodeValue} onChange={(value) => set("barcodeValue", value)} hint="Barcode assignments are immutable; archive and add a replacement if needed." />
        {values.barcodeValue && <SelectField label="Barcode type" name="barcodeNamespace" value={values.barcodeNamespace} onChange={(value) => set("barcodeNamespace", value)} options={[["gtin", "GTIN"], ["internal", "Internal"]]} />}
        <label className="check-field catalog-span"><input type="checkbox" checked={values.configureDefaultPolicy} onChange={(event) => set("configureDefaultPolicy", event.target.checked)} /> Enable purchasing and selling; make this the default Pack</label>
      </FormSection>}
      {candidates?.length ? <div className="duplicate-warning catalog-span" role="alert"><strong>Possible similar product already exists.</strong><p>Review these advisory matches. They do not imply generic, clinical, or substitution equivalence.</p><ul>{candidates.map((candidate) => <li key={candidate.candidateId}><Link to={`/app/products/${candidate.candidateId}`}>{candidateNames.data?.find((item) => item.id === candidate.candidateId)?.displayName ?? candidate.candidateId}</Link> — {candidate.reasonCodes.map(reasonLabel).join(", ")}</li>)}</ul><button className="button button--secondary" type="button" onClick={() => { setConfirmed(true); setCandidates(candidates); }}>Continue with legitimate creation</button></div> : null}
      {!record && context.isError && <InlineQueryError label="Store information could not be loaded. It is required to create a Product." onRetry={() => void context.refetch()} />}
      {notice && <div className="inline-notice inline-notice--error catalog-span" role="alert">{isRevisionConflict(mutation.error) && record ? STALE_RECORD_MESSAGE : notice}{isRevisionConflict(mutation.error) && record && <button type="button" onClick={() => void reloadLatest()} disabled={reloading}>{reloading ? "Reloading…" : "Reload latest"}</button>}</div>}
      <div className="form-actions catalog-span"><Link className="button button--secondary" to={record ? `/app/products/${record.id}` : "/app/products"}>Cancel</Link><button className="button button--primary" type="submit" disabled={mutation.isPending}>{mutation.isPending ? "Saving…" : candidates?.length && confirmed ? "Create Product Anyway" : record ? "Save Product" : "Review & Create Product"}</button></div>
    </form>
  </>;
}

export function ProductDetailPage() {
  const { id } = useParams();
  const auth = useAuth(); const canMutate = auth.status?.user?.role === "owner_admin";
  const queryClient = useQueryClient();
  const product = useQuery({ queryKey: ["product", id], queryFn: () => getProduct(id!), enabled: Boolean(id), retry: false });
  const references = useCatalogReferences();
  const [dialog, setDialog] = useState<DialogState>(null);
  const [managedPackId, setManagedPackId] = useState<string | null>(null);
  useExpireOnAuthError(product.error);
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["product", id] });
  // Authoritative reload for every revision conflict raised inside this workspace: invalidate the
  // Product query, await the refetch, and hand the caller the latest record to repopulate from.
  const reloadProduct = async () => { await refresh(); return queryClient.getQueryData<ProductDetail>(["product", id]) ?? null; };
  usePageTitle(product.data?.displayName ?? "Product");
  if (!id) return <Navigate to="/app/products" replace />;
  if (product.isPending) return <Loading label="Loading product…" />;
  if (product.isError) return <QueryError label="Product could not be loaded" onRetry={() => void product.refetch()} />;
  const record = product.data;
  const refState = referenceState(references);
  const unit = referenceName(references.data?.units, record.baseUnitId, refState);
  return <>
    <nav className="breadcrumbs" aria-label="Breadcrumb"><Link to="/app/products">Products</Link><span aria-hidden="true">/</span><span>{record.displayName}</span></nav>
    <PageHeading eyebrow="PRODUCT CATALOG" title={record.displayName} description={`${kindLabel(record.productKind)} · ${unit}`}>
      <div className="page-actions">{canMutate && record.status === "active" && <Link className="button button--secondary" to={`/app/products/${record.id}/edit`}>Edit Product</Link>}{canMutate && <button className="button button--primary" type="button" onClick={() => setDialog({ kind: "product-lifecycle" })}>{record.status === "active" ? "Archive Product" : "Restore Product"}</button>}</div>
    </PageHeading>
    {references.isError && <InlineQueryError label="Brand, Dosage Form, Company, and Unit names could not be loaded." onRetry={() => void references.refetch()} />}
    <section className="catalog-overview" aria-label="Product overview"><Info label="Status"><Status value={record.status} /></Info><Info label="Brand">{referenceName(references.data?.brands, record.brandId, refState)}</Info><Info label="Dosage Form">{referenceName(references.data?.dosageForms, record.dosageFormId, refState)}</Info><Info label="Quantity precision">{record.quantityScale === 0 ? "Whole units" : `${record.quantityScale} decimal places`}</Info><Info label="Formulation">{record.formulationDescriptor || "—"}</Info><Info label="Route / release">{[record.routeDescriptor, record.releaseDescriptor].filter(Boolean).join(" · ") || "—"}</Info></section>
    <CatalogSection title="Company Roles" description="A Product may have multiple manufacturers, marketers, brand owners, and importers." action={canMutate && record.status === "active" ? <button className="button button--secondary" type="button" onClick={() => setDialog({ kind: "role" })}>Add Company Role</button> : undefined}>
      <RoleTable roles={record.companyRoles} companies={references.data?.companies} referenceState={refState} canMutate={canMutate} onEdit={(role) => setDialog({ kind: "role", roleId: role.id })} onLifecycle={(role) => setDialog({ kind: "role-lifecycle", roleId: role.id })} />
    </CatalogSection>
    <CatalogSection title="Packs & SKUs" description={`Conversions are authoritative in ${unit} units; no stock quantities are stored here.`} action={canMutate && record.status === "active" ? <button className="button button--secondary" type="button" onClick={() => setDialog({ kind: "pack" })}>Add Pack</button> : undefined}>
      <PackTable product={record} units={references.data?.units} referenceState={refState} canMutate={canMutate} managedPackId={managedPackId} onManage={setManagedPackId} onEdit={(pack) => setDialog({ kind: "pack", packId: pack.id })} onLifecycle={(pack) => setDialog({ kind: "pack-lifecycle", packId: pack.id })} />
      {managedPackId && <PackWorkspace product={record} pack={record.packs.find((pack) => pack.id === managedPackId)!} canMutate={canMutate} onPolicy={() => setDialog({ kind: "policy", packId: managedPackId })} onBarcode={() => setDialog({ kind: "barcode", packId: managedPackId })} onRefresh={refresh} />}
    </CatalogSection>
    {dialog?.kind === "role" && <RoleDialog product={record} roleId={dialog.roleId} onClose={() => setDialog(null)} onSaved={() => { setDialog(null); void refresh(); }} onReload={reloadProduct} />}
    {dialog?.kind === "pack" && <PackDialog product={record} packId={dialog.packId} units={references.data?.units} referenceState={refState} onClose={() => setDialog(null)} onSaved={() => { setDialog(null); void refresh(); }} onReload={reloadProduct} />}
    {dialog?.kind === "policy" && <PolicyDialog product={record} packId={dialog.packId} onClose={() => setDialog(null)} onSaved={() => { const packId = dialog.packId; setDialog(null); void queryClient.invalidateQueries({ queryKey: ["pack-policy", packId] }); void refresh(); }} />}
    {dialog?.kind === "barcode" && <BarcodeDialog packId={dialog.packId} onClose={() => setDialog(null)} onSaved={() => { const packId = dialog.packId; setDialog(null); void queryClient.invalidateQueries({ queryKey: ["pack-barcodes", packId] }); void refresh(); }} />}
    {isLifecycleDialog(dialog) && <LifecycleDialog state={dialog} target={lifecycleTarget(dialog, record)} onClose={() => setDialog(null)} onSaved={() => { setDialog(null); void refresh(); }} onReload={reloadProduct} />}
  </>;
}

type DialogState = { kind: "role"; roleId?: string } | { kind: "pack"; packId?: string } | { kind: "policy" | "barcode"; packId: string } | LifecycleState | null;
type LifecycleState = { kind: "product-lifecycle" } | { kind: "role-lifecycle"; roleId: string } | { kind: "pack-lifecycle"; packId: string };
type LifecycleTarget = Product | ProductCompanyRole | ProductPack;

function isLifecycleDialog(state: DialogState): state is LifecycleState {
  return state !== null && ["product-lifecycle", "role-lifecycle", "pack-lifecycle"].includes(state.kind);
}
// Resolved from the live Product query on every render, so a reloaded record immediately supplies
// the revision the next lifecycle attempt sends.
function lifecycleTarget(state: LifecycleState, product: ProductDetail): LifecycleTarget | undefined {
  if (state.kind === "product-lifecycle") return product;
  if (state.kind === "role-lifecycle") return product.companyRoles.find((role) => role.id === state.roleId);
  return product.packs.find((pack) => pack.id === state.packId);
}

function RoleTable({ roles, companies, referenceState, canMutate, onEdit, onLifecycle }: { roles: ProductCompanyRole[]; companies: ReferenceMasterResponse[] | undefined; referenceState: ReferenceState; canMutate: boolean; onEdit: (role: ProductCompanyRole) => void; onLifecycle: (role: ProductCompanyRole) => void }) {
  if (!roles.length) return <InlineEmpty text="No company roles configured." />;
  return <div className="table-scroll"><table className="data-table"><thead><tr><th scope="col">Company</th><th scope="col">Role</th><th scope="col">Effective period</th><th scope="col">Status</th><th scope="col"><span className="sr-only">Actions</span></th></tr></thead><tbody>{roles.map((role) => <tr key={role.id}><td data-label="Company">{referenceName(companies, role.companyId, referenceState)}</td><td data-label="Role">{ROLE_LABELS[role.role]}</td><td data-label="Effective period">{role.effectiveFrom || "Open"} → {role.effectiveTo || "Open"}</td><td data-label="Status"><Status value={role.status} /></td><td className="row-actions"><div>{canMutate && role.status === "active" && <button type="button" onClick={() => onEdit(role)}>Edit</button>}{canMutate && <button type="button" onClick={() => onLifecycle(role)}>{role.status === "active" ? "Archive" : "Restore"}</button>}</div></td></tr>)}</tbody></table></div>;
}

function PackTable({ product, units, referenceState, canMutate, managedPackId, onManage, onEdit, onLifecycle }: { product: ProductDetail; units: ReferenceMasterResponse[] | undefined; referenceState: ReferenceState; canMutate: boolean; managedPackId: string | null; onManage: (id: string | null) => void; onEdit: (pack: ProductPack) => void; onLifecycle: (pack: ProductPack) => void }) {
  if (!product.packs.length) return <InlineEmpty text="No Packs configured." />;
  return <div className="table-scroll"><table className="data-table"><thead><tr><th scope="col">Pack</th><th scope="col">Contains</th><th scope="col">Containment</th><th scope="col">SKU</th><th scope="col">Status</th><th scope="col"><span className="sr-only">Actions</span></th></tr></thead><tbody>{product.packs.map((pack) => { const child = product.packs.find((item) => item.id === pack.containedPackId); return <tr key={pack.id}><td data-label="Pack">{pack.displayLabel || referenceName(units, pack.containerUnitId, referenceState)}</td><td data-label="Contains">{atomsToQuantity(pack.baseQuantityAtoms, product.quantityScale)} {referenceName(units, product.baseUnitId, referenceState)}</td><td data-label="Containment">{child ? `${pack.containedPackCount} × ${child.displayLabel || referenceName(units, child.containerUnitId, referenceState)}` : "Direct"}</td><td data-label="SKU">{pack.skuCode || "—"}</td><td data-label="Status"><Status value={pack.status} /></td><td className="row-actions"><div><button type="button" onClick={() => onManage(managedPackId === pack.id ? null : pack.id)}>{managedPackId === pack.id ? "Close management" : "Manage"}</button>{canMutate && pack.status === "active" && <button type="button" onClick={() => onEdit(pack)}>Edit</button>}{canMutate && <button type="button" onClick={() => onLifecycle(pack)}>{pack.status === "active" ? "Archive" : "Restore"}</button>}</div></td></tr>; })}</tbody></table></div>;
}

function PackWorkspace({ product, pack, canMutate, onPolicy, onBarcode, onRefresh }: { product: ProductDetail; pack: ProductPack; canMutate: boolean; onPolicy: () => void; onBarcode: () => void; onRefresh: () => void }) {
  const policy = useQuery({ queryKey: ["pack-policy", pack.id], queryFn: () => getPackPolicy(pack.id), retry: false });
  const barcodes = useQuery({ queryKey: ["pack-barcodes", pack.id], queryFn: () => listBarcodes(pack.id), retry: false });
  const lifecycle = useMutation({ mutationFn: ({ barcode, action }: { barcode: Barcode; action: "archive" | "restore" }) => changeBarcodeLifecycle(barcode, action, action === "archive" ? "No longer used" : "Required again"), onSuccess: () => { void barcodes.refetch(); onRefresh(); } });
  const policyLifecycle = useMutation({ mutationFn: ({ policy, action }: { policy: StorePackPolicy; action: "archive" | "restore" }) => changePackPolicyLifecycle(policy, action, action === "archive" ? "Policy no longer used" : "Policy required again"), onSuccess: () => { void policy.refetch(); onRefresh(); } });
  return <div className="pack-workspace"><div className="pack-workspace__header"><div><strong>{pack.displayLabel || "Pack management"}</strong><p>Store policy and immutable barcode assignments.</p></div>{canMutate && pack.status === "active" && <div><button className="button button--secondary" type="button" onClick={onPolicy}>{policy.data ? "Edit Policy" : "Add Policy"}</button><button className="button button--secondary" type="button" onClick={onBarcode}>Add Barcode</button></div>}</div>
    <div className="pack-management-grid"><section><h3>Store Pack Policy</h3>{policy.isPending ? <p role="status">Loading store policy…</p> : policy.isError ? <InlineQueryError label="Store policy could not be loaded." onRetry={() => void policy.refetch()} /> : policy.data ? <><dl className="compact-list"><div><dt>Purchase</dt><dd>{policy.data.purchaseEnabled ? "Enabled" : "Disabled"}{policy.data.defaultPurchasePack ? " · Default" : ""}</dd></div><div><dt>Sale</dt><dd>{policy.data.saleEnabled ? "Enabled" : "Disabled"}{policy.data.defaultSalePack ? " · Default" : ""}</dd></div><div><dt>Minimum sale</dt><dd>{atomsToQuantity(policy.data.minimumSaleIncrementAtoms, product.quantityScale)}</dd></div></dl><div className="policy-status-actions"><Status value={policy.data.status} />{canMutate && <button type="button" onClick={() => policyLifecycle.mutate({ policy: policy.data!, action: policy.data!.status === "active" ? "archive" : "restore" })}>{policy.data.status === "active" ? "Archive Policy" : "Restore Policy"}</button>}</div>{policyLifecycle.error && <MutationError error={policyLifecycle.error} onReload={() => void policy.refetch()} reloading={policy.isRefetching} />}</> : <p>No store policy configured.</p>}</section>
      <section><h3>Barcodes</h3>{barcodes.isPending ? <p role="status">Loading barcodes…</p> : barcodes.isError ? <InlineQueryError label="Barcodes could not be loaded." onRetry={() => void barcodes.refetch()} /> : barcodes.data.length ? <ul className="barcode-list">{barcodes.data.map((barcode) => <li key={barcode.id}><span><strong>{barcode.normalizedValue}</strong><small>{barcode.namespace.toUpperCase()} · {barcode.scope} · {barcode.symbology || "Unspecified symbology"}</small></span><Status value={barcode.status} />{canMutate && <button type="button" onClick={() => lifecycle.mutate({ barcode, action: barcode.status === "active" ? "archive" : "restore" })}>{barcode.status === "active" ? "Archive" : "Restore"}</button>}</li>)}</ul> : <p>No barcodes assigned.</p>}{lifecycle.error && <MutationError error={lifecycle.error} onReload={() => void barcodes.refetch()} reloading={barcodes.isRefetching} />}</section></div>
  </div>;
}

function RoleDialog({ product, roleId, onClose, onSaved, onReload }: { product: ProductDetail; roleId?: string; onClose: () => void; onSaved: () => void; onReload: ReloadProduct }) {
  const record = roleId ? product.companyRoles.find((role) => role.id === roleId) : undefined;
  const [values, setValues] = useState(() => roleValues(record));
  const [staleNotice, setStaleNotice] = useState<string | null>(null);
  const mutation = useMutation({ mutationFn: () => { const value = { companyId: values.companyId, role: values.role, effectiveFrom: values.from || null, effectiveTo: values.to || null }; return record ? updateCompanyRole(record, value) : createCompanyRole(product.id, value); }, onSuccess: onSaved });
  const reload = useReloadLatest(async () => {
    const latest = await onReload();
    const role = roleId && latest ? latest.companyRoles.find((item) => item.id === roleId) : undefined;
    if (!latest || (roleId && !role)) { setStaleNotice("The latest version of this Company Role could not be loaded. Cancel and reopen it."); return; }
    setValues(roleValues(role)); setStaleNotice(null); mutation.reset();
  });
  return <CatalogDialog title={record ? "Edit Company Role" : "Add Company Role"} onClose={onClose}><form className="master-form" onSubmit={(event) => { event.preventDefault(); if (values.companyId) mutation.mutate(); }}><ReferencePicker label="Company" kind="companies" value={values.companyId} onChange={(companyId) => setValues({ ...values, companyId })} /><SelectField label="Role" name="role" value={values.role} onChange={(value) => setValues({ ...values, role: value as ProductCompanyRoleFields["role"] })} options={Object.entries(ROLE_LABELS).map(([value, label]) => [value, label] as const)} /><TextField label="Effective from" name="effectiveFrom" type="date" value={values.from} onChange={(from) => setValues({ ...values, from })} /><TextField label="Effective to" name="effectiveTo" type="date" value={values.to} onChange={(to) => setValues({ ...values, to })} />{staleNotice && <div className="inline-notice inline-notice--error" role="alert">{staleNotice}</div>}{mutation.error && <MutationError error={mutation.error} onReload={() => void reload.run()} reloading={reload.pending} />}<DialogActions onClose={onClose} pending={mutation.isPending} /></form></CatalogDialog>;
}

function PackDialog({ product, packId, units, referenceState, onClose, onSaved, onReload }: { product: ProductDetail; packId?: string; units: ReferenceMasterResponse[] | undefined; referenceState: ReferenceState; onClose: () => void; onSaved: () => void; onReload: ReloadProduct }) {
  const record = packId ? product.packs.find((pack) => pack.id === packId) : undefined;
  const context = useCatalogContext();
  const [values, setValues] = useState(() => packValues(record, product.quantityScale));
  const [error, setError] = useState<string | null>(null);
  const child = product.packs.find((pack) => pack.id === values.containedId); const expectedAtoms = child && Number.isInteger(Number(values.count)) && Number(values.count) > 0 ? child.baseQuantityAtoms * Number(values.count) : null;
  // Phase 1B invariant: a Pack carries sku_store_id exactly when it carries sku_code, so a non-blank
  // SKU can only be saved once the authenticated Catalog context has supplied the current Store.
  const scopedSku = values.sku.trim();
  const mutation = useMutation({ mutationFn: () => { const atoms = quantityToAtoms(values.quantity, product.quantityScale); if (!atoms) throw new Error("Enter a positive direct base quantity."); if (expectedAtoms !== null && atoms !== expectedAtoms) throw new Error(`Direct quantity must equal ${atomsToQuantity(expectedAtoms, product.quantityScale)}.`); const pack: ProductPackFields = { containerUnitId: values.unitId, baseQuantityAtoms: atoms, containedPackId: values.containedId || null, containedPackCount: values.containedId ? Number(values.count) : null, ...skuFields(values.sku, context.data?.storeId ?? null), displayLabel: values.label || null }; return record ? updatePack(record, pack) : createPack(product.id, pack); }, onSuccess: onSaved, onError: (caught) => setError(caught instanceof LocalServiceError ? caught.message : caught instanceof Error ? caught.message : "Pack could not be saved.") });
  const reload = useReloadLatest(async () => {
    const latest = await onReload();
    const pack = packId && latest ? latest.packs.find((item) => item.id === packId) : undefined;
    if (!latest || (packId && !pack)) { setError("The latest version of this Pack could not be loaded. Cancel and reopen it."); return; }
    setValues(packValues(pack, latest.quantityScale)); setError(null); mutation.reset();
  });
  const submit = (event: FormEvent) => {
    event.preventDefault(); setError(null);
    if (!values.unitId) { setError("Select a Pack Unit."); return; }
    if (scopedSku && !context.data) { setError(context.isError ? "The current Store could not be identified, so a Store-scoped SKU cannot be saved. Retry Store information or clear the SKU." : "Store information is still loading. Try again."); return; }
    mutation.mutate();
  };
  return <CatalogDialog title={record ? "Edit Pack" : "Add Pack"} description={`Base unit: ${referenceName(units, product.baseUnitId, referenceState)}`} onClose={onClose}><form className="master-form" onSubmit={submit}><ReferencePicker label="Pack Unit" kind="units" value={values.unitId} onChange={(unitId) => setValues({ ...values, unitId })} /><TextField label="Direct base quantity" name="baseQuantity" value={values.quantity} onChange={(quantity) => setValues({ ...values, quantity })} required /><SelectField label="Contained Pack (optional)" name="containedPack" value={values.containedId} onChange={(containedId) => setValues({ ...values, containedId })} options={[["", "Direct conversion"], ...product.packs.filter((pack) => pack.id !== record?.id && pack.status === "active").map((pack) => [pack.id, pack.displayLabel || referenceName(units, pack.containerUnitId, referenceState)] as const)]} />{values.containedId && <TextField label="Contained Pack count" name="containedPackCount" type="number" value={values.count} onChange={(count) => setValues({ ...values, count })} required hint={expectedAtoms ? `Expected base quantity: ${atomsToQuantity(expectedAtoms, product.quantityScale)}` : undefined} />}<TextField label="Pack label" name="displayLabel" value={values.label} onChange={(label) => setValues({ ...values, label })} /><TextField label="SKU (optional)" name="skuCode" value={values.sku} onChange={(sku) => setValues({ ...values, sku })} hint="A SKU is recorded against the current Store." />{context.isError && <InlineQueryError label="Store information could not be loaded. It is required to save a SKU." onRetry={() => void context.refetch()} />}{error && <div className="inline-notice inline-notice--error" role="alert">{isRevisionConflict(mutation.error) ? STALE_RECORD_MESSAGE : error}{isRevisionConflict(mutation.error) && <button type="button" onClick={() => void reload.run()} disabled={reload.pending}>{reload.pending ? "Reloading…" : "Reload latest"}</button>}</div>}<DialogActions onClose={onClose} pending={mutation.isPending || context.isPending} /></form></CatalogDialog>;
}

function PolicyDialog({ product, packId, onClose, onSaved }: { product: ProductDetail; packId: string; onClose: () => void; onSaved: () => void }) {
  const queryClient = useQueryClient();
  const context = useCatalogContext(); const current = useQuery({ queryKey: ["pack-policy", packId], queryFn: () => getPackPolicy(packId), retry: false });
  const reloadPolicy = async () => { await queryClient.invalidateQueries({ queryKey: ["pack-policy", packId] }); return { loaded: queryClient.getQueryState(["pack-policy", packId])?.status === "success", policy: queryClient.getQueryData<StorePackPolicy | null>(["pack-policy", packId]) ?? null }; };
  if (context.isPending || current.isPending) return <CatalogDialog title="Store Pack Policy" onClose={onClose}><Loading label="Loading policy…" /></CatalogDialog>;
  if (context.isError || current.isError) return <CatalogDialog title="Store Pack Policy" onClose={onClose}><QueryError label="Policy could not be loaded" onRetry={() => { if (context.isError) void context.refetch(); if (current.isError) void current.refetch(); }} /></CatalogDialog>;
  return <PolicyForm product={product} packId={packId} storeId={context.data.storeId} current={current.data} onClose={onClose} onSaved={onSaved} onReload={reloadPolicy} />;
}

function PolicyForm({ product, packId, storeId, current, onClose, onSaved, onReload }: { product: ProductDetail; packId: string; storeId: string; current: StorePackPolicy | null; onClose: () => void; onSaved: () => void; onReload: () => Promise<{ loaded: boolean; policy: StorePackPolicy | null }> }) {
  const [latest, setLatest] = useState(current);
  const [values, setValues] = useState(() => policyValues(current, product.quantityScale));
  const [notice, setNotice] = useState<string | null>(null);
  const mutation = useMutation({ mutationFn: () => { const atoms = quantityToAtoms(values.minimum, product.quantityScale); if (!atoms) throw new Error("Minimum sale increment must be positive."); if (values.defaultPurchase && !values.purchase) throw new Error("The default purchase Pack must be purchase-enabled."); if (values.defaultSale && !values.sale) throw new Error("The default sale Pack must be sale-enabled."); const policy: StorePackPolicyFields = { storeId, purchaseEnabled: values.purchase, saleEnabled: values.sale, wholePackOnlyPurchase: values.wholePurchase, fractionalSaleAllowed: product.quantityScale > 0 && values.fractional, minimumSaleIncrementAtoms: atoms, defaultPurchasePack: values.defaultPurchase, defaultSalePack: values.defaultSale }; return savePackPolicy(packId, latest, policy); }, onSuccess: onSaved, onError: (error) => setNotice(error instanceof LocalServiceError ? error.message : error instanceof Error ? error.message : "Policy could not be saved.") });
  const reload = useReloadLatest(async () => {
    const { loaded, policy } = await onReload();
    if (!loaded) { setNotice("The latest Store Pack Policy could not be loaded. Try again."); return; }
    setLatest(policy); setValues(policyValues(policy, product.quantityScale)); setNotice(null); mutation.reset();
  });
  return <CatalogDialog title={latest ? "Edit Store Pack Policy" : "Add Store Pack Policy"} onClose={onClose}><form className="master-form" onSubmit={(event) => { event.preventDefault(); setNotice(null); mutation.mutate(); }}><Check label="Purchase enabled" checked={values.purchase} onChange={(purchase) => setValues({ ...values, purchase })} /><Check label="Sale enabled" checked={values.sale} onChange={(sale) => setValues({ ...values, sale })} /><Check label="Whole-pack-only purchase" checked={values.wholePurchase} onChange={(wholePurchase) => setValues({ ...values, wholePurchase })} /><Check label="Fractional sale allowed" checked={values.fractional} onChange={(fractional) => setValues({ ...values, fractional })} disabled={product.quantityScale === 0} hint={product.quantityScale === 0 ? "Whole-unit products cannot sell fractional atoms." : undefined} /><TextField label="Minimum sale increment" name="minimumSaleIncrement" value={values.minimum} onChange={(minimum) => setValues({ ...values, minimum })} required /><Check label="Default purchase Pack" checked={values.defaultPurchase} onChange={(defaultPurchase) => setValues({ ...values, defaultPurchase })} /><Check label="Default sale Pack" checked={values.defaultSale} onChange={(defaultSale) => setValues({ ...values, defaultSale })} />{notice && <div className="inline-notice inline-notice--error" role="alert">{isRevisionConflict(mutation.error) ? STALE_RECORD_MESSAGE : notice}{isRevisionConflict(mutation.error) && <button type="button" onClick={() => void reload.run()} disabled={reload.pending}>{reload.pending ? "Reloading…" : "Reload latest"}</button>}</div>}<DialogActions onClose={onClose} pending={mutation.isPending} /></form></CatalogDialog>;
}

function BarcodeDialog({ packId, onClose, onSaved }: { packId: string; onClose: () => void; onSaved: () => void }) {
  const context = useCatalogContext(); const [namespace, setNamespace] = useState("gtin"); const [value, setValue] = useState(""); const [symbology, setSymbology] = useState(""); const [scope, setScope] = useState<"global" | "store">("global"); const [notice, setNotice] = useState<string | null>(null);
  const storeScoped = namespace === "internal" || scope === "store";
  const mutation = useMutation({ mutationFn: () => createBarcode(packId, { namespace, value, symbology: symbology || null, scope: namespace === "internal" ? "store" : scope, storeId: storeScoped ? context.data!.storeId : null }), onSuccess: onSaved });
  const submit = (event: FormEvent) => {
    event.preventDefault(); setNotice(null);
    if (!value.trim()) { setNotice("Enter a barcode value."); return; }
    // A Store-scoped Barcode cannot be submitted without the authenticated Catalog context; the
    // failure is reported instead of silently discarding the submission.
    if (storeScoped && !context.data) { setNotice(context.isError ? "The current Store could not be identified, so a Store-scoped Barcode cannot be saved. Retry Store information or use Global scope." : "Store information is still loading. Try again."); return; }
    mutation.mutate();
  };
  return <CatalogDialog title="Add Pack Barcode" description="Barcode values cannot be reassigned or edited. Archive and add a replacement if necessary." onClose={onClose}><form className="master-form" onSubmit={submit}><SelectField label="Namespace / type" name="namespace" value={namespace} onChange={(next) => { setNamespace(next); if (next === "internal") setScope("store"); }} options={[["gtin", "GTIN"], ["internal", "Internal"], ["code128", "Code 128"]]} /><TextField label="Barcode value" name="value" value={value} onChange={setValue} required /><TextField label="Symbology (optional)" name="symbology" value={symbology} onChange={setSymbology} /><SelectField label="Scope" name="scope" value={namespace === "internal" ? "store" : scope} onChange={(next) => setScope(next as "global" | "store")} disabled={namespace === "internal"} options={[["global", "Global"], ["store", "This store"]]} />{context.isError && <InlineQueryError label="Store information could not be loaded. It is required for a Store-scoped Barcode." onRetry={() => void context.refetch()} />}{notice && <div className="inline-notice inline-notice--error" role="alert">{notice}</div>}{mutation.error && <MutationError error={mutation.error} />}<DialogActions onClose={onClose} pending={mutation.isPending || context.isPending} /></form></CatalogDialog>;
}

function LifecycleDialog({ state, target, onClose, onSaved, onReload }: { state: LifecycleState; target: LifecycleTarget | undefined; onClose: () => void; onSaved: () => void; onReload: ReloadProduct }) {
  const active = target?.status === "active"; const action = active ? "archive" : "restore"; const [reason, setReason] = useState(active ? "" : "Required again"); const [staleNotice, setStaleNotice] = useState<string | null>(null);
  const mutation = useMutation<unknown, Error, void>({ mutationFn: () => { if (!target) throw new Error("This record is no longer available."); return state.kind === "product-lifecycle" ? changeProductLifecycle(target as Product, action, reason) : state.kind === "role-lifecycle" ? changeCompanyRoleLifecycle(target as ProductCompanyRole, action, reason) : changePackLifecycle(target as ProductPack, action, reason); }, onSuccess: onSaved });
  // The lifecycle target is derived from the Product query on every render, so awaiting the refetch
  // is enough to arm the next attempt with the current revision rather than the stale one.
  const reload = useReloadLatest(async () => {
    const latest = await onReload();
    if (!latest) { setStaleNotice("The latest version of this record could not be loaded. Cancel and reopen it."); return; }
    setStaleNotice(null); mutation.reset();
  });
  if (!target) return <CatalogDialog title="Record unavailable" onClose={onClose}><div className="master-form"><div className="inline-notice inline-notice--error" role="alert">This record is no longer part of the Product. Close and review the latest Product.</div><div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Close</button></div></div></CatalogDialog>;
  return <CatalogDialog title={`${active ? "Archive" : "Restore"} record`} description={active ? "Archiving preserves identity and history. Product archive requires active child records to be archived first." : "Restore reruns current integrity and uniqueness checks."} onClose={onClose}><form className="master-form" onSubmit={(event) => { event.preventDefault(); if (reason.trim()) mutation.mutate(); }}><TextField label="Reason" name="reason" value={reason} onChange={setReason} required />{staleNotice && <div className="inline-notice inline-notice--error" role="alert">{staleNotice}</div>}{mutation.error && <MutationError error={mutation.error} onReload={() => void reload.run()} reloading={reload.pending} />}<div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={!reason.trim() || mutation.isPending}>{mutation.isPending ? "Saving…" : active ? "Archive record" : "Restore record"}</button></div></form></CatalogDialog>;
}

function useCatalogReferences() {
  return useQuery({ queryKey: ["catalog", "references"], queryFn: async () => { const [units, dosageForms, brands, companies] = await Promise.all([listReferences("units", "", "active"), listReferences("dosage-forms", "", "active"), listReferences("brands", "", "active"), listReferences("companies", "", "active")]); return { units, dosageForms, brands, companies }; }, staleTime: 30_000, retry: false });
}

function useCatalogContext() {
  return useQuery({ queryKey: ["catalog-context"], queryFn: getCatalogContext, retry: false });
}

// Serialises an awaited authoritative refetch so "Reload latest" can report progress and can never
// be issued twice concurrently.
function useReloadLatest(reload: () => Promise<void>) {
  const [pending, setPending] = useState(false);
  return { pending, run: async () => { if (pending) return; setPending(true); try { await reload(); } finally { setPending(false); } } };
}

function ReferencePicker({ label, kind, value, onChange, optional = false, error }: { label: string; kind: ReferenceKind; value: string; onChange: (value: string, record?: ReferenceMasterResponse) => void; optional?: boolean; error?: string }) {
  const [search, setSearch] = useState(""); const debounced = useDebouncedValue(search, 250); const query = useQuery({ queryKey: ["reference-picker", kind, debounced], queryFn: () => listReferences(kind, debounced, "active"), retry: false }); const id = `catalog-${label.toLowerCase().replace(/\W+/g, "-")}`; const fieldName = ({ Brand: "brandId", "Dosage Form": "dosageFormId", "Base Unit": "baseUnitId", "Pack Unit": "initialPackUnitId", Company: "companyId" } as Record<string, string>)[label] ?? label.replace(/\s+/g, "").replace(/^./, (letter) => letter.toLowerCase());
  return <div className="field reference-picker"><label htmlFor={`${id}-search`}>Search {label}</label><input id={`${id}-search`} value={search} onChange={(event) => setSearch(event.target.value)} placeholder={`Type to filter ${label.toLowerCase()}`} /><label htmlFor={id}>{label}{!optional && <span aria-hidden="true"> *</span>}</label><select id={id} name={fieldName} value={value} onChange={(event) => onChange(event.target.value, query.data?.find((item) => item.id === event.target.value))} aria-invalid={Boolean(error)} disabled={query.isPending}><option value="">{query.isPending ? "Loading…" : query.isError ? `${label} options unavailable` : optional ? `No ${label.toLowerCase()} selected` : `Select ${label}`}</option>{query.data?.map((item) => <option key={item.id} value={item.id}>{referencePrimaryName(item)}</option>)}</select>{query.isError && <span className="field-error reference-picker__error" role="alert">Options could not be loaded.<button type="button" onClick={() => void query.refetch()}>Retry {label}</button></span>}{!query.isPending && !query.isError && query.data?.length === 0 && <small>No matching options.</small>}{error && <small className="field-error">{error}</small>}</div>;
}

function productValues(record?: ProductDetail): ProductValues { return { productKind: record?.productKind ?? "medicine", brandId: record?.brandId ?? "", dosageFormId: record?.dosageFormId ?? "", baseUnitId: record?.baseUnitId ?? "", quantityScale: String(record?.quantityScale ?? 0), displayName: record?.displayName ?? "", formulationDescriptor: record?.formulationDescriptor ?? "", routeDescriptor: record?.routeDescriptor ?? "", releaseDescriptor: record?.releaseDescriptor ?? "", initialPackUnitId: "", initialPackQuantity: "1", initialPackLabel: "", initialSku: "", companyId: "", companyRole: "manufacturer", barcodeNamespace: "gtin", barcodeValue: "", configureDefaultPolicy: true }; }
function productFields(values: ProductValues): ProductFields { return { productKind: values.productKind, brandId: values.brandId || null, dosageFormId: values.dosageFormId || null, baseUnitId: values.baseUnitId, quantityScale: Number(values.quantityScale), displayName: values.displayName, formulationDescriptor: values.formulationDescriptor || null, routeDescriptor: values.routeDescriptor || null, releaseDescriptor: values.releaseDescriptor || null }; }
function roleValues(record?: ProductCompanyRole) { return { companyId: record?.companyId ?? "", role: record?.role ?? ("manufacturer" as ProductCompanyRoleFields["role"]), from: record?.effectiveFrom ?? "", to: record?.effectiveTo ?? "" }; }
function packValues(record: ProductPack | undefined, quantityScale: number) { return { unitId: record?.containerUnitId ?? "", quantity: record ? atomsToQuantity(record.baseQuantityAtoms, quantityScale) : "1", containedId: record?.containedPackId ?? "", count: record?.containedPackCount ? String(record.containedPackCount) : "", label: record?.displayLabel ?? "", sku: record?.skuCode ?? "" }; }
function policyValues(record: StorePackPolicy | null, quantityScale: number) { return { purchase: record?.purchaseEnabled ?? true, sale: record?.saleEnabled ?? true, wholePurchase: record?.wholePackOnlyPurchase ?? true, fractional: quantityScale > 0 && (record?.fractionalSaleAllowed ?? false), minimum: record ? atomsToQuantity(record.minimumSaleIncrementAtoms, quantityScale) : "1", defaultPurchase: record?.defaultPurchasePack ?? false, defaultSale: record?.defaultSalePack ?? false }; }
// Phase 1B invariant: sku_store_id is present exactly when sku_code is present. A blank or
// whitespace-only SKU normalizes to no SKU on the Store Service, so it must clear both fields.
function skuFields(raw: string, storeId: string | null): Pick<ProductPackFields, "skuCode" | "skuStoreId"> { const value = raw.trim(); return value && storeId ? { skuCode: value, skuStoreId: storeId } : { skuCode: null, skuStoreId: null }; }
function createRequest(values: ProductValues, product: ProductFields, storeId: string): CreateProductRequest { const atoms = quantityToAtoms(values.initialPackQuantity, product.quantityScale)!; return { product, companyRoles: values.companyId ? [{ companyId: values.companyId, role: values.companyRole, effectiveFrom: null, effectiveTo: null }] : [], packs: [{ clientKey: "initial-pack", containerUnitId: values.initialPackUnitId, baseQuantityAtoms: atoms, containedPackId: null, containedPackCount: null, ...skuFields(values.initialSku, storeId), displayLabel: values.initialPackLabel || null, policy: values.configureDefaultPolicy ? { storeId, purchaseEnabled: true, saleEnabled: true, wholePackOnlyPurchase: true, fractionalSaleAllowed: false, minimumSaleIncrementAtoms: 1, defaultPurchasePack: true, defaultSalePack: true } : null, barcodes: values.barcodeValue ? [{ namespace: values.barcodeNamespace, value: values.barcodeValue, symbology: null, scope: values.barcodeNamespace === "internal" ? "store" : "global", storeId: values.barcodeNamespace === "internal" ? storeId : null }] : [] }] }; }
function validateProduct(values: ProductValues, editing: boolean): FieldErrors { const errors: FieldErrors = {}; if (!values.displayName.trim()) errors.displayName = "Display name is required."; if (!values.baseUnitId) errors.baseUnitId = "Select a Base Unit."; if (values.productKind === "medicine" && !values.dosageFormId) errors.dosageFormId = "Medicine requires a Dosage Form."; const scale = Number(values.quantityScale); if (!Number.isInteger(scale) || scale < 0 || scale > 6) errors.quantityScale = "Use a whole number from 0 to 6."; if (!editing) { if (!values.initialPackUnitId) errors.initialPackUnitId = "Select a Pack Unit."; if (quantityToAtoms(values.initialPackQuantity, Number.isInteger(scale) ? scale : 0) === null) errors.initialPackQuantity = "Enter a positive quantity using the approved precision."; } return errors; }

function FormSection({ title, description, children }: { title: string; description: string; children: ReactNode }) { return <fieldset className="catalog-form__section"><legend>{title}</legend><p>{description}</p><div>{children}</div></fieldset>; }
function TextField({ label, name, value, onChange, required, type = "text", error, hint }: { label: string; name: string; value: string; onChange: (value: string) => void; required?: boolean; type?: string; error?: string; hint?: string }) { const id = `catalog-${name}`; const help = error || hint ? `${id}-help` : undefined; return <div className="field"><label htmlFor={id}>{label}{required && <span aria-hidden="true"> *</span>}</label><input id={id} name={name} type={type} value={value} onChange={(event) => onChange(event.target.value)} required={required} aria-invalid={Boolean(error)} aria-describedby={help} />{help && <small id={help} className={error ? "field-error" : ""}>{error || hint}</small>}</div>; }
function SelectField({ label, name, value, onChange, options, error, disabled }: { label: string; name: string; value: string; onChange: (value: string) => void; options: ReadonlyArray<readonly [string, string]>; error?: string; disabled?: boolean }) { const id = `catalog-${name}`; return <div className="field"><label htmlFor={id}>{label}</label><select id={id} name={name} value={value} onChange={(event) => onChange(event.target.value)} aria-invalid={Boolean(error)} disabled={disabled}>{options.map(([option, text]) => <option key={option} value={option}>{text}</option>)}</select>{error && <small className="field-error">{error}</small>}</div>; }
function Check({ label, checked, onChange, disabled, hint }: { label: string; checked: boolean; onChange: (value: boolean) => void; disabled?: boolean; hint?: string }) { return <label className="check-field"><input type="checkbox" checked={checked} onChange={(event) => onChange(event.target.checked)} disabled={disabled} /> <span>{label}{hint && <small>{hint}</small>}</span></label>; }
function DialogActions({ onClose, pending }: { onClose: () => void; pending: boolean }) { return <div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={pending}>{pending ? "Saving…" : "Save"}</button></div>; }
function MutationError({ error, onReload, reloading = false }: { error: Error; onReload?: () => void; reloading?: boolean }) { const local = error instanceof LocalServiceError ? error : null; const stale = local?.code === "revision_conflict" && Boolean(onReload); return <div className="inline-notice inline-notice--error" role="alert">{stale ? STALE_RECORD_MESSAGE : local?.message ?? "The Local Store Service could not complete this request."}{stale && <button type="button" onClick={onReload} disabled={reloading}>{reloading ? "Reloading…" : "Reload latest"}</button>}</div>; }
function PageHeading({ eyebrow, title, description, children }: { eyebrow: string; title: string; description: string; children?: ReactNode }) { return <header className="page-header"><div><p className="eyebrow">{eyebrow}</p><h1>{title}</h1><p>{description}</p></div>{children}</header>; }
function CatalogSection({ title, description, action, children }: { title: string; description: string; action?: ReactNode; children: ReactNode }) { return <section className="catalog-section"><header><div><h2>{title}</h2><p>{description}</p></div>{action}</header>{children}</section>; }
function Info({ label, children }: { label: string; children: ReactNode }) { return <div><dt>{label}</dt><dd>{children}</dd></div>; }
function Status({ value }: { value: string }) { return <span className={`status-badge status-badge--${value}`}>{value === "active" ? "Active" : "Archived"}</span>; }
function Loading({ label }: { label: string }) { return <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>{label}</b></div>; }
function QueryError({ label, onRetry }: { label: string; onRetry: () => void }) { return <div className="empty-state" role="alert"><h3>{label}</h3><p>The Local Store Service did not complete this request.</p><button className="button button--secondary" type="button" onClick={onRetry}>Retry</button></div>; }
function Empty({ search, archived }: { search: boolean; archived: boolean }) { return <div className="empty-state"><h3>{search ? "No matching products" : archived ? "No archived products" : "No products yet"}</h3><p>{search ? "Clear or change the search to see more results." : "Create the first Product when its identity and Pack are known."}</p></div>; }
function InlineEmpty({ text }: { text: string }) { return <div className="catalog-inline-empty">{text}</div>; }
// A failed query is never rendered as a valid empty business state: it keeps its own safe message
// and its own retry action.
function InlineQueryError({ label, onRetry }: { label: string; onRetry: () => void }) { return <div className="catalog-inline-error" role="alert"><span>{label}</span><button className="button button--secondary" type="button" onClick={onRetry}>Retry</button></div>; }
function kindLabel(kind: ProductFields["productKind"]) { return PRODUCT_KINDS.find(([value]) => value === kind)?.[1] ?? kind; }
function referencePrimaryName(record: ReferenceMasterResponse) { const attrs = record.attributes as Record<string, unknown>; return String(attrs.displayName ?? attrs.canonicalCode ?? attrs.hsnCode ?? record.id); }
function referenceState(query: { isPending: boolean; isError: boolean }): ReferenceState { return query.isPending ? "pending" : query.isError ? "error" : "ready"; }
// A reference-label lookup that failed reports itself as unavailable rather than pretending to be
// still loading; the owning page carries the retry action.
function referenceName(records: ReferenceMasterResponse[] | undefined, id: string | null | undefined, state: ReferenceState = "ready") { if (!id) return "—"; if (records) return referencePrimaryName(records.find((record) => record.id === id) ?? ({ id, attributes: {} } as ReferenceMasterResponse)); return state === "error" ? "Name unavailable" : "Loading…"; }
function reasonLabel(reason: string) { return ({ display_name_match: "same display name", brand_match: "same brand", dosage_form_match: "same dosage form", formulation_descriptor_match: "same formulation descriptor", route_descriptor_match: "same route descriptor", release_descriptor_match: "same release descriptor", company_match: "same company", pack_conversion_match: "same Pack conversion", barcode_match: "same barcode" } as Record<string, string>)[reason] ?? "similar catalog attributes"; }
function useDebouncedValue(value: string, wait: number) { const [debounced, setDebounced] = useState(value); useEffect(() => { const timer = window.setTimeout(() => setDebounced(value), wait); return () => window.clearTimeout(timer); }, [value, wait]); return debounced; }
function usePageTitle(title: string) { useEffect(() => { const previous = document.title; document.title = `${title} | AUSHADHARTH`; return () => { document.title = previous; }; }, [title]); }
function useExpireOnAuthError(error: Error | null) { const auth = useAuth(); useEffect(() => { if (error instanceof LocalServiceError && ["authentication_required", "session_expired"].includes(error.code)) void auth.expireSession(); }, [error]); }
