# How it works

Deep mode: what the site's JavaScript is made of and what its start page did while we watched.

## Its JavaScript

Analysed 2 script(s) from http://127.0.0.1:8123/#apps: 2 module(s), 1 endpoint(s), 0 route(s).

Evidence ev_84ddf7f8292058bda54344b9abe6be2b1dc74e7691ba73ac08d95da697f6bb53.

```json
{
  "api_operations": [],
  "app_kind": "unknown",
  "bundlers": [],
  "edges": [
    {
      "from": "event:127.0.0.1:8123/static/app.js#add:click",
      "kind": "handled-by",
      "to": "fn:127.0.0.1:8123/static/app.js#add.addEventListener(click)@582"
    },
    {
      "from": "event:127.0.0.1:8123/static/app.js#b:click",
      "kind": "handled-by",
      "to": "fn:127.0.0.1:8123/static/app.js#b.addEventListener(click)@325"
    },
    {
      "from": "event:127.0.0.1:8123/static/app.js#document:click",
      "kind": "handled-by",
      "to": "fn:127.0.0.1:8123/static/app.js#document.addEventListener(click)@586"
    },
    {
      "from": "event:127.0.0.1:8123/static/app.js#document:keydown",
      "kind": "handled-by",
      "to": "fn:127.0.0.1:8123/static/app.js#document.addEventListener(keydown)@619"
    },
    {
      "from": "event:127.0.0.1:8123/static/app.js#pill:click",
      "kind": "handled-by",
      "to": "fn:127.0.0.1:8123/static/app.js#pill.addEventListener(click)@577"
    },
    {
      "from": "event:127.0.0.1:8123/static/app.js#t:click",
      "kind": "handled-by",
      "to": "fn:127.0.0.1:8123/static/app.js#t.addEventListener(click)@421"
    },
    {
      "from": "event:127.0.0.1:8123/static/chart.umd.min.js#window:resize",
      "kind": "handled-by",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#vs@19"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#add.addEventListener(click)@582",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#$@2"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#appRowsSorted@442",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#applyFocus@431",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#$@2"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#areaPath@260",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#flush@270"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#areaPath@260",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#b.addEventListener(click)@325",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#refresh@595"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#b.addEventListener(click)@325",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#setTab@330"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#document.addEventListener(keydown)@619",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#applyFocus@431"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#fmt@68",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#gpuTemp@131",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#j@69",
      "kind": "requests",
      "to": "endpoint:{url}"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#loadTileConfig@38",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#defaultTileConfig@37"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#loadTileConfig@38",
      "kind": "uses",
      "to": "storage:localStorage.getItem syswatch-tile-config"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#mb@368",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#pointsVsRange@118",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#fmt@68"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#pointsVsRange@118",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#pointsVsRange@118",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#rangeWord@111"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#pushLive@145",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#pushLiveSeries@149",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#batPct@78"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#pushLiveSeries@149",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#gpuUtil@77"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#pushLiveSeries@149",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#pushLiveSeries@149",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#pushLive@145"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#pushLiveSeries@149",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#workloadLoad@96"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#refresh@595",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#applyFocus@431"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#refresh@595",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#j@69"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#refresh@595",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#pushLiveSeries@149"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#refresh@595",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#renderAnalytics@558"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#refresh@595",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#renderAppsTab@478"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#refresh@595",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#renderRanges@318"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#refresh@595",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#renderStatus@483"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#refresh@595",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#renderTiles@388"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#refresh@595",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#renderTopApps@473"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#render@616",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#refresh@595"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderAnalytics@558",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#$@2"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderAppsTab@478",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#$@2"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderAppsTab@478",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#appRowsSorted@442"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderAppsTab@478",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#appsHtml@460"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderRanges@318",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#$@2"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderStatus@483",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#$@2"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderStatus@483",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#esc@66"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderStatus@483",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#fmt@68"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderStatus@483",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderStatus@483",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#ramPct@76"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderTiles@388",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#$@2"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderTiles@388",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#visibleTiles@58"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderTopApps@473",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#$@2"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderTopApps@473",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#appRowsSorted@442"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#renderTopApps@473",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#appsHtml@460"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#saveTileConfig@53",
      "kind": "uses",
      "to": "storage:localStorage.setItem syswatch-tile-config"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#seriesBounds@248",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#at@254"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#setupDeviceMenu@573",
      "kind": "listens",
      "to": "event:127.0.0.1:8123/static/app.js#add:click"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#setupDeviceMenu@573",
      "kind": "listens",
      "to": "event:127.0.0.1:8123/static/app.js#document:click"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#setupDeviceMenu@573",
      "kind": "listens",
      "to": "event:127.0.0.1:8123/static/app.js#pill:click"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#setupDeviceMenu@573",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#$@2"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#sparkPath@290",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#sparkSvg@309",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#sparkPath@290"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#storageThroughputMB@90",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#t.addEventListener(click)@421",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#applyFocus@431"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileDelta@179",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#cpuOf@75"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileDelta@179",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#fmt@68"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileDelta@179",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileDelta@179",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#pointsVsRange@118"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileDelta@179",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#ramPct@76"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileDelta@179",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#seriesOf@127"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileSub@157",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#fmt@68"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileSub@157",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileValue@97",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#batPct@78"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileValue@97",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#cpuOf@75"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileValue@97",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#fmt@68"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileValue@97",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#gpuUtil@77"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileValue@97",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileValue@97",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#maxTemp@82"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileValue@97",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#netDown@81"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileValue@97",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#pkgW@79"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileValue@97",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#ramPct@76"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileValue@97",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#storageThroughputMB@90"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileValue@97",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#workloadLoad@96"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#tileVisible@54",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#defaultTileConfig@37"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#timeLabels@235",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#fmt@68"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#u@371",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#fmt@68"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#u@371",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#visibleTiles@58",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#defaultTileConfig@37"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideFooter@357",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#esc@66"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideFooter@357",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#fmt@68"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideFooter@357",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#gpuTemp@131"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideFooter@357",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#isNum@67"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideFooter@357",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#maxTemp@82"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideFooter@357",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#mb@368"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideFooter@357",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#netDown@81"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideFooter@357",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#u@371"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideSeries@221",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#tileSeries@197"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideSparkSvg@342",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#areaPath@260"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideSparkSvg@342",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#seriesBounds@248"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideSparkSvg@342",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#sparkPath@290"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideSparkSvg@342",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#timeLabels@235"
    },
    {
      "from": "fn:127.0.0.1:8123/static/app.js#wideSparkSvg@342",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/app.js#wideSeries@221"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#$e@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#Ye@19"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#$i@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#ji@19"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#$i@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#s@13"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#$n._getCircumference@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#$@13"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#$n._getRingWeight@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#l@13"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#$n._getRotation@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#$@13"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#$n.getLabelAndValue@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#ne@19"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#$n.parse@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#o@13"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#$n.parse@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#r@13"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#$n.update@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#c@13"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#$n.update@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#h@13"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#$s.hasValue@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#N@13"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#<class>._calculateBarIndexPixels@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#l@13"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#<class>._calculateBarValuePixels@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#Bn@19"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#<class>._calculateBarValuePixels@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#s@13"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#<class>._getRuler@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#zn@19"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#<class>._getStacks@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#h@13"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#<class>.draw@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#Gn@19"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#<class>.draw@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/static/chart.umd.min.js#Le@19"
    },
    {
      "from": "fn:127.0.0.1:8123/static/chart.umd.min.js#<class>.draw@19",
      "kind": "calls",
      "to": "fn:127.0.0.1:8123/stat
```

## Ten seconds of it

Watched http://127.0.0.1:8123/#apps for 10000ms: 70 request outline(s), 35 console entr(y/ies).

Evidence ev_df1bb6824de8ba268573a52cd3f55994f32123d181a332d45597ad78f082599b.

```json
{
  "accessibility": null,
  "console": {
    "count": 35,
    "entries": [
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      },
      {
        "level": "error",
        "source": "network"
      }
    ],
    "has_text": false
  },
  "dom": {
    "attribute_names": [
      "aria-expanded",
      "aria-hidden",
      "aria-label",
      "charset",
      "class",
      "content",
      "cx",
      "cy",
      "d",
      "data-device",
      "data-range",
      "data-tab",
      "data-tile",
      "fill",
      "height",
      "hidden",
      "href",
      "id",
      "lang",
      "name",
      "preserveaspectratio",
      "r",
      "rel",
      "role",
      "src",
      "stroke",
      "stroke-linecap",
      "stroke-linejoin",
      "stroke-width",
      "style",
      "tabindex",
      "title",
      "vector-effect",
      "viewbox",
      "width"
    ],
    "elements_by_name": {
      "body": 1,
      "button": 26,
      "circle": 1,
      "div": 355,
      "head": 1,
      "header": 1,
      "html": 1,
      "link": 1,
      "main": 1,
      "meta": 2,
      "nav": 1,
      "path": 25,
      "script": 2,
      "section": 5,
      "span": 1228,
      "svg": 16,
      "title": 1
    },
    "text_nodes": 677,
    "total_nodes": 2347,
    "truncated": false
  },
  "dropped_events": 0,
  "frame_origins": [
    "http://127.0.0.1:8123"
  ],
  "network": {
    "request_count": 70,
    "requests": [
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/current"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/history?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/analytics?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/consumers?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/sensors"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/thermal"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/battery"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/current"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/history?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/analytics?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/consumers?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/sensors"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/thermal"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/battery"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/current"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/history?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/analytics?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/consumers?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/sensors"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/thermal"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/battery"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/current"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/history?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/analytics?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/consumers?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/sensors"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/thermal"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/battery"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/current"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/history?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/analytics?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/consumers?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/sensors"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/thermal"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/battery"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/current"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/history?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/analytics?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/consumers?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/sensors"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/thermal"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/battery"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/current"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/history?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/analytics?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/consumers?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/sensors"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/thermal"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/battery"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/current"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/history?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/analytics?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/consumers?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/sensors"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/thermal"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/battery"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/current"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/history?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/analytics?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/consumers?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/sensors"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/thermal"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/battery"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/current"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/history?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/analytics?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/consumers?range=1h"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/sensors"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/thermal"
      },
      {
        "initiator": "script",
        "method": "GET",
        "type": "Fetch",
        "url": "http://127.0.0.1:8123/api/battery"
      }
    ]
  },
  "origin": "http://127.0.0.1:8123/#apps",
  "resources": [
    {
      "mime_type": "text/javascript",
      "type": "Script",
      "url": "http://127.0.0.1:8123/static/chart.umd.min.js"
    },
    {
      "mime_type": "text/javascript",
      "type": "Script",
      "url": "http://127.0.0.1:8123/static/app.js"
    }
  ],
  "scripts": [
    {
      "length": 206944,
      "source_map_url": "chart.umd.js.map",
      "url": "http://127.0.0.1:8123/static/chart.umd.min.js"
    },
    {
      "length": 29941,
      "source_map_url": null,
      "url": "http://127.0.0.1:8123/static/app.js"
    }
  ],
  "scripts_truncated": false,
  "secret_redactions": 0,
  "storage_keys": null,
  "watch_ms": 10000,
  "websockets": {
    "connections": [],
    "frames": []
  }
}
```
