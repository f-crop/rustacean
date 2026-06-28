import { useEffect, useRef, useState } from "react";
import { Check, ChevronDown } from "lucide-react";
import { cn } from "@/lib/utils";
import { useMe } from "@/api/hooks/useMe";
import { useSwitchTenant } from "@/api/hooks/useSwitchTenant";

export function TenantSwitcher(): JSX.Element | null {
  const { data: me } = useMe();
  const switchTenant = useSwitchTenant();
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!open) return;

    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") {
        setOpen(false);
        triggerRef.current?.focus();
      }
    };

    const onOutsideClick = (e: MouseEvent): void => {
      if (
        containerRef.current &&
        !containerRef.current.contains(e.target as Node)
      ) {
        setOpen(false);
      }
    };

    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onOutsideClick);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onOutsideClick);
    };
  }, [open]);

  if (!me) return null;

  const { current_tenant, available_tenants } = me;
  const canSwitch = available_tenants.length > 1;

  const handleSelect = (tenantId: string): void => {
    if (tenantId === current_tenant.id) {
      setOpen(false);
      return;
    }
    switchTenant.mutate({ tenant_id: tenantId });
    setOpen(false);
  };

  if (!canSwitch) {
    return (
      <span
        className="max-w-[120px] truncate rounded-md border border-border px-2 py-1 text-sm text-muted-foreground"
        data-testid="current-tenant-name"
      >
        {current_tenant.name}
      </span>
    );
  }

  return (
    <div ref={containerRef} className="relative">
      <button
        ref={triggerRef}
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={`Current tenant: ${current_tenant.name}. Click to switch.`}
        onClick={() => setOpen((v) => !v)}
        data-testid="tenant-switcher-trigger"
        className="flex items-center gap-1 rounded-md border border-border bg-background px-2 py-1 text-sm hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <span className="max-w-[120px] truncate" data-testid="tenant-current-name">
          {current_tenant.name}
        </span>
        <ChevronDown
          aria-hidden="true"
          className={cn(
            "h-3 w-3 shrink-0 transition-transform",
            open && "rotate-180",
          )}
        />
      </button>

      {open && (
        <ul
          role="listbox"
          aria-label="Select tenant"
          data-testid="tenant-switcher-menu"
          className="absolute right-0 top-full z-50 mt-1 min-w-[160px] rounded-md border border-border bg-background py-1 shadow-md"
        >
          {available_tenants.map((tenant) => {
            const isCurrent = tenant.id === current_tenant.id;
            return (
              <li key={tenant.id} role="option" aria-selected={isCurrent}>
                <button
                  type="button"
                  onClick={() => handleSelect(tenant.id)}
                  data-testid={`tenant-option-${tenant.slug}`}
                  className={cn(
                    "flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm hover:bg-accent",
                    isCurrent && "font-medium",
                  )}
                >
                  <span className="flex-1 truncate">{tenant.name}</span>
                  {isCurrent && (
                    <Check
                      aria-hidden="true"
                      className="h-3 w-3 shrink-0 text-primary"
                    />
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
