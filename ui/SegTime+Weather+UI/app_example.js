/* Application example update logic based on fonts-e.html with BetterClock offset hook */

var daylist = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
var clockOffsetMs = 0.0;
var clockStarted = false;
var forecastStarted = false;
var BASE_CLOCK_W = 306;
var BASE_CLOCK_H = 123;
var FORECAST_REFRESH_MS = 15 * 60 * 1000;
var FORECAST_RETRY_MS = 3000;
var FORECAST_BOX_COUNT = 7;
var FORECAST_VIEW_SWITCH_MS = 20000;
var forecastViewMode = "weekly";
var forecastBundle = { weekly: [], hourly_today: [] };
var instanceLabelLoaded = false;

function genTimerStrings(tm, num) {
  var i;
  var ret = tm.toString(10);
  var left = ret.length;

  if (left < num) {
    for (i = 0; i < (num - left); i++) {
      ret = String(0) + ret;
    }
  }

  return ret;
}

function correctedDate() {
  return new Date(Date.now() + clockOffsetMs);
}

function escapeHtml(text) {
  return String(text)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#039;");
}

async function refreshOffset() {
  if (window.pywebview && window.pywebview.api) {
    try {
      var ms = await window.pywebview.api.get_offset_ms();
      if (typeof ms === "number" && isFinite(ms)) {
        clockOffsetMs = ms;
      }
    } catch (_err) {
      // Keep local timing until bridge is reachable.
    }
  }
  setTimeout(refreshOffset, 2000);
}

function renderForecast(bundle) {
  var strip = document.getElementById("forecastStrip");
  if (!strip) return;

  var html = "";
  if (forecastViewMode === "hourly") {
    var hourly = Array.isArray(bundle.hourly_today) ? bundle.hourly_today.slice(0, FORECAST_BOX_COUNT) : [];
    while (hourly.length < FORECAST_BOX_COUNT) {
      hourly.push({ hour: "--", icon: ":", temp: "--", unit: "" });
    }
    for (var h = 0; h < hourly.length; h++) {
      var hourItem = hourly[h] || {};
      var hour = escapeHtml(hourItem.hour || "--");
      var hourIcon = escapeHtml(hourItem.icon || ":");
      var hourTemp = escapeHtml(hourItem.temp || "--");
      var hourUnit = escapeHtml(hourItem.unit || "");
      html +=
        "<div class=\"forecast-box\">" +
        "<div class=\"forecast-day\">" + hour + "</div>" +
        "<div class=\"forecast-icon\">" + hourIcon + "</div>" +
        "<div class=\"forecast-lch-label\">CURRENT</div>" +
        "<div class=\"forecast-temp\">" + hourTemp + hourUnit + "</div>" +
        "</div>";
    }
  } else {
    var weekly = Array.isArray(bundle.weekly) ? bundle.weekly.slice(0, FORECAST_BOX_COUNT) : [];
    while (weekly.length < FORECAST_BOX_COUNT) {
      weekly.push({
        day: "--",
        icon: ":",
        low: "--",
        current: "",
        high: "--",
        is_today: false,
      });
    }
    for (var i = 0; i < weekly.length; i++) {
      var item = weekly[i] || {};
      var day = escapeHtml(item.day || "--");
      var icon = escapeHtml(item.icon || ":");
      var low = escapeHtml(item.low || "--");
      var current = escapeHtml(item.current || "");
      var high = escapeHtml(item.high || "--");
      var isToday = !!item.is_today && current !== "";
      var label = isToday ? "LOW : CURRENT : HIGH" : "LOW : HIGH";
      var value = isToday ? (low + " : " + current + " : " + high) : (low + " : " + high);
      html +=
        "<div class=\"forecast-box\">" +
        "<div class=\"forecast-day\">" + day + "</div>" +
        "<div class=\"forecast-icon\">" + icon + "</div>" +
        "<div class=\"forecast-lch-label\">" + label + "</div>" +
        "<div class=\"forecast-temp\">" + value + "</div>" +
        "</div>";
    }
  }

  strip.innerHTML = html;
}

async function refreshForecast() {
  if (window.pywebview && window.pywebview.api) {
    try {
      var data = await window.pywebview.api.get_nws_weather();
      if (data && Array.isArray(data.weekly)) {
        forecastBundle = {
          weekly: data.weekly || [],
          hourly_today: Array.isArray(data.hourly_today) ? data.hourly_today : [],
        };
        renderForecast(forecastBundle);
        setTimeout(refreshForecast, FORECAST_REFRESH_MS);
        return;
      }
    } catch (_err) {
      // Try again soon while bridge/api settles.
    }
    setTimeout(refreshForecast, FORECAST_RETRY_MS);
    return;
  }
  // Bridge not ready yet; retry quickly instead of waiting 15 minutes.
  setTimeout(refreshForecast, FORECAST_RETRY_MS);
}

function startForecast() {
  if (forecastStarted) {
    return;
  }
  forecastStarted = true;
  renderForecast(forecastBundle);
  refreshForecast();
  setInterval(function () {
    forecastViewMode = forecastViewMode === "weekly" ? "hourly" : "weekly";
    renderForecast(forecastBundle);
  }, FORECAST_VIEW_SWITCH_MS);
}

function updateTimer() {
  var date = correctedDate();
  var tm_year, tm_mon, tm_date, tm_hour, tm_min, tm_sec, tm_msec, tm_day;
  var colon;

  tm_year = date.getFullYear();
  tm_mon = date.getMonth() + 1;
  tm_date = date.getDate();
  tm_day = date.getDay();
  tm_hour = date.getHours();
  tm_min = date.getMinutes();
  tm_sec = date.getSeconds();
  tm_msec = date.getMilliseconds();

  tm_mon = genTimerStrings(tm_mon, 2);
  tm_date = genTimerStrings(tm_date, 2);
  tm_hour = genTimerStrings(tm_hour, 2);
  tm_min = genTimerStrings(tm_min, 2);
  tm_sec = genTimerStrings(tm_sec, 2);
  tm_day = daylist[tm_day];

  if (tm_msec > 499) {
    colon = " ";
  } else {
    colon = ":";
  }

  document.getElementById("DSEGClock").innerHTML =
    tm_hour + colon + tm_min + "<span class=\"Clock-Sec\">" + tm_sec + "</span>";
  document.getElementById("DSEGClock-Year").innerHTML =
    "<span class=\"D7MI\">" + tm_year + "-" + tm_mon + "-" + tm_date + " " +
    "</span><span class=\"D14MI\">" + tm_day + "." + "</span>";

  setTimeout("updateTimer()", 500 - date.getMilliseconds() % 500);
}

function applyScale() {
  var center = document.querySelector(".center");
  if (!center) return;
  var styles = window.getComputedStyle(center);
  var padLeft = parseFloat(styles.paddingLeft) || 0;
  var padRight = parseFloat(styles.paddingRight) || 0;
  var padTop = parseFloat(styles.paddingTop) || 0;
  var padBottom = parseFloat(styles.paddingBottom) || 0;
  var usableW = Math.max(1, center.clientWidth - padLeft - padRight);
  var usableH = Math.max(1, center.clientHeight - padTop - padBottom);
  var scaleW = usableW / BASE_CLOCK_W;
  var scaleH = usableH / BASE_CLOCK_H;
  var scale = scaleW;
  if (BASE_CLOCK_H * scale > usableH) {
    scale = scaleH;
  }
  scale = Math.max(0.35, scale);
  center.style.setProperty("--clock-scale", String(scale));
}

async function toggleFullscreen() {
  if (window.pywebview && window.pywebview.api) {
    try {
      await window.pywebview.api.toggle_fullscreen();
    } catch (_err) {
      // Ignore if platform backend denies fullscreen toggle.
    }
  }
}

async function refreshInstanceLabel() {
  if (instanceLabelLoaded) {
    return;
  }
  var label = document.getElementById("instanceLabel");
  if (!label) {
    return;
  }
  if (window.pywebview && window.pywebview.api) {
    try {
      var instanceId = await window.pywebview.api.get_instance_id();
      if (instanceId) {
        label.textContent = "instance: " + instanceId;
        instanceLabelLoaded = true;
        return;
      }
    } catch (_err) {
      // Retry until bridge is available.
    }
  }
  setTimeout(refreshInstanceLabel, FORECAST_RETRY_MS);
}

function startClock() {
  if (clockStarted) {
    return;
  }
  clockStarted = true;
  applyScale();
  updateTimer();
  refreshOffset();
  startForecast();
  refreshInstanceLabel();
}

window.addEventListener("resize", function () {
  applyScale();
});

window.addEventListener("keydown", function (event) {
  if (event.key === "F1") {
    event.preventDefault();
    toggleFullscreen();
  }
});

window.addEventListener("pywebviewready", function () {
  refreshOffset();
  startForecast();
  refreshInstanceLabel();
});
