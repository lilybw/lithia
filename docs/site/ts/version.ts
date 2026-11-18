// Forwards a docs page. On a latest page, `?v=<version>` opens that version's copy under
// `versions/<version>/`; a stub page's `data-forward` opens its target. Both keep query and fragment.
((): void => {
    const version: string = "__LITHIA_VERSION__";
    const meta: HTMLMetaElement | null = document.querySelector<HTMLMetaElement>(
        'meta[name="rustdoc-vars"], meta[name="lithia-site"]',
    );
    if (meta === null) {
        return;
    }
    const root: URL = new URL(meta.dataset["rootPath"] ?? "./", location.href);
    const section: RegExpMatchArray | null = root.pathname.match(/^(.*\/)(api|spec)\/$/);
    const site: string = section?.[1] ?? "";
    const part: string = section?.[2] ?? "";
    const latest: boolean = section !== null && !/\/versions\/[^/]+\/$/.test(site);
    const params: URLSearchParams = new URLSearchParams(location.search);
    const wanted: string = (params.get("v") ?? "").replace(/^v/, "");
    if (latest && wanted !== "" && wanted !== version && /^\d+(\.\d+)*$/.test(wanted)) {
        params.delete("v");
        const query: string = params.toString();
        const rest: string = location.pathname.slice(root.pathname.length);
        location.replace(`${site}versions/${wanted}/${part}/${rest}${query === "" ? "" : `?${query}`}${location.hash}`);
        // rustdoc rewrites the URL as it starts a search, which would cancel the navigation.
        history.pushState = (): void => {};
        history.replaceState = (): void => {};
        document.documentElement.style.visibility = "hidden";
        return;
    }
    const forward: string | undefined = meta.dataset["forward"];
    if (forward !== undefined) {
        location.replace(new URL(forward, location.href).pathname + location.search + location.hash);
    }
})();
