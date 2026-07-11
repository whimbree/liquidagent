// liquid service worker: push display + click-to-focus. No offline caching
// yet — the shell is small and the supervisor is on the same box/LAN.
self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(clients.claim()));

self.addEventListener("push", (event) => {
  let data = { title: "liquid", body: "" };
  try { data = event.data.json(); } catch { /* keep defaults */ }
  event.waitUntil(
    self.registration.showNotification(data.title || "liquid", {
      body: data.body || "",
      icon: "/icon.svg",
      badge: "/icon.svg",
    })
  );
});

// Android share-target: "share a screenshot → liquid" from any app. The SW
// intercepts the manifest-declared POST /share, stashes the images in a
// cache, and redirects into the shell, which moves them into the chat
// composer (shell.js pickupSharedImages).
self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (event.request.method !== "POST" || url.pathname !== "/share") return;
  event.respondWith(
    (async () => {
      const form = await event.request.formData();
      const files = form.getAll("images").filter((f) => f instanceof File);
      const cache = await caches.open("liquid-shared-images");
      await Promise.all(
        files.map((f, i) =>
          cache.put(`/__shared/${i}`, new Response(f, { headers: { "Content-Type": f.type } }))
        )
      );
      return Response.redirect(`/?shared=${files.length}`, 303);
    })()
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil(
    clients.matchAll({ type: "window", includeUncontrolled: true }).then((list) => {
      for (const client of list) {
        if ("focus" in client) return client.focus();
      }
      return clients.openWindow("/");
    })
  );
});
