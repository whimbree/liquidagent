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
