(function() {
  var style = getComputedStyle(document.documentElement);
  var accent = style.getPropertyValue('--accent').trim();
  var accent2 = style.getPropertyValue('--accent2').trim();
  var ink = style.getPropertyValue('--ink').trim();
  var muted = style.getPropertyValue('--muted').trim();
  var rule = style.getPropertyValue('--rule').trim();
  var bg2 = style.getPropertyValue('--bg2').trim();

  // --- Chart: 线程扩展曲线（48 张样张两次冷跑均值） ---
  var el = document.getElementById('chart-threads');
  if (el) {
    var chart = echarts.init(el, null, { renderer: 'svg' });
    var threads = ['1', '2', '3', '4', '6', '8', '12'];
    var rates = [10.9, 16.5, 21.8, 30.2, 37.1, 40.9, 29.1];
    chart.setOption({
      animation: false,
      grid: { left: 56, right: 24, top: 46, bottom: 44 },
      tooltip: {
        appendToBody: true,
        trigger: 'axis',
        axisPointer: { type: 'shadow' },
        formatter: function(params) {
          var p = params[0];
          return '线程数 ' + p.name + '<br/>吞吐 ' + p.value.toFixed(1) + ' 张/秒';
        }
      },
      xAxis: {
        type: 'category',
        data: threads,
        name: '线程数',
        nameLocation: 'middle',
        nameGap: 30,
        nameTextStyle: { color: muted, fontSize: 12 },
        axisLine: { lineStyle: { color: rule } },
        axisTick: { show: false },
        axisLabel: { color: ink, fontFamily: 'JetBrainsMono, monospace' }
      },
      yAxis: {
        type: 'value',
        name: '吞吐（张/秒）',
        nameTextStyle: { color: muted, fontSize: 12, align: 'right' },
        splitLine: { lineStyle: { color: rule } },
        axisLabel: { color: muted, fontFamily: 'JetBrainsMono, monospace' }
      },
      series: [{
        type: 'bar',
        data: rates.map(function(v, i) {
          var t = threads[i];
          var color = t === '6' ? accent : (t === '12' ? muted : accent + '80');
          return {
            value: v,
            itemStyle: {
              color: color,
              borderRadius: [4, 4, 0, 0]
            },
            label: {
              show: true,
              position: 'top',
              color: t === '6' ? accent : (t === '12' ? muted : ink),
              fontWeight: t === '6' ? 700 : 400,
              fontFamily: 'JetBrainsMono, monospace',
              fontSize: 11,
              formatter: function(p) { return p.value.toFixed(1); }
            }
          };
        }),
        barWidth: '52%',
        markLine: {
          silent: true,
          symbol: 'none',
          lineStyle: { color: accent2, type: 'dashed', width: 1.5 },
          label: {
            formatter: '默认值 = 物理核数（6）',
            color: accent2,
            fontSize: 11,
            position: 'insideEndTop'
          },
          data: [{ xAxis: '6' }]
        }
      }]
    });
    window.addEventListener('resize', function() { chart.resize(); });
  }
})();
