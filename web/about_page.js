import { ABOUT_DATA } from "./about-data.js";

/* The About page: app identity, the components the build actually ships with
 * their licences, and the contributors. The component list is generated
 * (tools/gen_about.mjs) so its versions cannot drift from the installed tree;
 * this module only turns that data into DOM and keeps the two languages in
 * step. Licence identifiers are not translated. */
const labels = {
  version: ["版本", "Version"],
  license: ["许可证", "Licence"],
  author: ["作者", "Author"],
  changes: ["改动", "Changes"],
  runtime: ["运行时（Web / 手机壳）", "Runtime (web and mobile shells)"],
  firmware: ["固件", "Firmware"],
  assets: ["素材", "Artwork"],
  groupTitles: { runtime: "runtime", firmware: "firmware", assets: "assets" },
};

export function createAboutPage({ section, getLang }) {
  const text = (key) => {
    const entry = labels[key];
    if (!entry) return key;
    return entry[getLang().startsWith("zh") ? 0 : 1];
  };
  const $ = (selector) => section.querySelector(selector);
  const componentsHost = $("#aboutComponents");
  const contributorsHost = $("#aboutContributors");

  function componentRow(component) {
    const row = document.createElement("div");
    row.className = "about-component";
    const name = document.createElement("a");
    name.className = "about-name";
    name.textContent = component.name;
    name.href = component.url;
    name.target = "_blank";
    name.rel = "noreferrer";
    const version = document.createElement("span");
    version.className = "about-version";
    version.textContent = component.version;
    const license = document.createElement("span");
    license.className = "about-license";
    license.textContent = component.license;
    row.append(name, version, license);
    // Artwork carries attribution and a statement of changes, which a licence
    // identifier alone does not express.
    if (component.author || component.changes) {
      const extra = document.createElement("span");
      extra.className = "about-extra";
      extra.textContent = [component.author && `${text("author")}: ${component.author}`,
        component.changes && `${text("changes")}: ${component.changes}`].filter(Boolean).join(" · ");
      row.append(extra);
    }
    if (component.licenseFile) {
      const link = document.createElement("a");
      link.className = "about-license-link";
      link.textContent = component.license;
      link.href = component.licenseFile;
      link.target = "_blank";
      link.rel = "noreferrer";
      row.replaceChild(link, license);
    }
    return row;
  }

  function render() {
    const app = ABOUT_DATA.app;
    $("#aboutMeta").textContent = `${app.name} · ${text("version")} ${app.version} · ${app.license}`;
    componentsHost.textContent = "";
    for (const group of ABOUT_DATA.groups) {
      const title = document.createElement("h4");
      title.className = "about-group-title";
      title.textContent = text(labels.groupTitles[group.id] || group.id);
      const list = document.createElement("div");
      list.className = "about-group";
      for (const component of group.components) list.append(componentRow(component));
      componentsHost.append(title, list);
    }
    contributorsHost.textContent = "";
    for (const name of ABOUT_DATA.contributors) {
      const chip = document.createElement("span");
      chip.className = "about-contributor";
      chip.textContent = name;
      contributorsHost.append(chip);
    }
    $("#aboutRepository").href = app.repository;
    $("#aboutIssues").href = app.issues;
    $("#aboutLicenseLink").href = app.licenseUrl;
  }

  render();
  return { refreshLang: render, render };
}
