package main

import (
	"context"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	statsService "github.com/xtls/xray-core/app/stats/command"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
)

// ================= CONFIG & CONSTANTS =================
var (
	scrapeInterval = 5 * time.Second
	failInterval   = 15 * time.Second
	rpcTimeout     = 3 * time.Second
)

// ================= METRICS (Gauge only) =================

var (
	// GaugeVec 用于记录动态列表，需要 Reset()
	xrayUserIPOnline = prometheus.NewGaugeVec(
		prometheus.GaugeOpts{
			Name: "xray_user_ip_online",
			Help: "User online status per IP (1=online)",
		},
		[]string{"name", "ip"},
	)

	// Gauge 用于记录单值状态，无需 Reset()
	xrayUp = prometheus.NewGauge(
		prometheus.GaugeOpts{
			Name: "xray_up",
			Help: "Whether Xray is reachable (1=up, 0=down)",
		},
	)
)

// ================= TRAFFIC COLLECTOR (CUSTOM) =================

// XrayTrafficCollector 实现了 prometheus.Collector 接口，用于采集累积流量。
type XrayTrafficCollector struct {
	client      statsService.StatsServiceClient
	trafficDesc *prometheus.Desc
}

func NewXrayTrafficCollector(client statsService.StatsServiceClient) *XrayTrafficCollector {
	// 遵循 Prometheus 最佳实践，Counter 命名以 _total 结尾
	return &XrayTrafficCollector{
		client: client,
		trafficDesc: prometheus.NewDesc(
			"xray_traffic_bytes_total",
			"Xray traffic statistics",
			[]string{"type", "name", "direction"}, // 标签：user/inbound/outbound, 对应的名称, up/down
			nil,
		),
	}
}

// Describe 将指标描述符发送给 Prometheus
func (c *XrayTrafficCollector) Describe(ch chan<- *prometheus.Desc) {
	ch <- c.trafficDesc
}

// Collect 查询 Xray API 并将结果作为 CounterValue 发送
func (c *XrayTrafficCollector) Collect(ch chan<- prometheus.Metric) {
	ctx, cancel := context.WithTimeout(context.Background(), rpcTimeout)
	defer cancel()

	// 1. 查询 Xray 统计数据
	resp, err := c.client.QueryStats(ctx, &statsService.QueryStatsRequest{
		Pattern: "",
		Reset_:  false,
	})
	if err != nil {
		log.Printf("TrafficCollector error during QueryStats: %v", err)
		// 不设置 xrayUp，由 scrapeLoop 负责
		return
	}

	// 2. 遍历并上报为 Counter
	for _, stat := range resp.Stat {
		if stat.Value == 0 {
			continue
		}
		// 确保只处理流量相关的统计项
		if strings.Contains(stat.Name, ">>>traffic>>>") {
			parts := strings.Split(stat.Name, ">>>")
			if len(parts) < 4 {
				continue
			}
			typ := parts[0]       // e.g., user
			nameLabel := parts[1] // e.g., username
			direction := parts[3] // e.g., up or down

			// 🌟 关键：使用 MustNewConstMetric 和 CounterValue
			ch <- prometheus.MustNewConstMetric(
				c.trafficDesc,
				prometheus.CounterValue, // 明确告诉 Prometheus 这是个 Counter
				float64(stat.Value),     // 直接上报 Xray API 返回的累积总数
				typ, nameLabel, direction,
			)
		}
	}
}

// ================= MAIN =================

func main() {
	log.Printf("Starting Xray exporter %s...\n", Version)

	// gRPC connection
	conn, err := grpc.NewClient(AppConfig.XrayApi, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		log.Fatal("Connect to Xray failed:", err)
	}
	defer conn.Close()
	client := statsService.NewStatsServiceClient(conn)

	// Prometheus registry
	reg := prometheus.NewRegistry()

	// 1. 注册自定义流量 Collector (处理 Counter)
	trafficCollector := NewXrayTrafficCollector(client)
	reg.MustRegister(trafficCollector)

	// 2. 注册 Gauge (处理在线IP和健康状态)
	reg.MustRegister(xrayUserIPOnline)
	reg.MustRegister(xrayUp)

	// Handle shutdown
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	// Start scrape loop (只负责在线 IP 列表和 xray_up 状态)
	go scrapeLoop(ctx, client)

	// Start HTTP server
	http.Handle("/metrics", promhttp.HandlerFor(reg, promhttp.HandlerOpts{}))
	addr := fmt.Sprintf(":%d", AppConfig.Port)
	log.Printf("Exporter listening on %s/metrics\n", addr)
	log.Fatal(http.ListenAndServe(addr, nil))
}

// ================= SCRAPE LOOP & FUNCTIONS =================

func scrapeLoop(ctx context.Context, client statsService.StatsServiceClient) {
	log.Println("Scrape loop started (single-thread mode)")

	failCount := 0

	// Initial delay
	time.Sleep(2 * time.Second)

	for {
		select {
		case <-ctx.Done():
			log.Println("Scrape loop stopped")
			return
		default:
			// 只采集 Gauge 类型指标（在线IP和xrayUp状态）
			err := scrapeOnlineUsersAndHealth(client)
			if err != nil {
				failCount++
				xrayUp.Set(0)
				log.Println("scrapeOnlineUsersAndHealth error:", err)
			} else {
				failCount = 0
				xrayUp.Set(1) // 只要能查询到用户，就认为 Xray 是健康的
			}

			// Adjust sleep
			sleep := scrapeInterval
			if failCount >= 3 {
				sleep = failInterval
			}
			time.Sleep(sleep)
		}
	}
}

// scrapeOnlineUsersAndHealth 采集在线 IP 列表和健康状态
func scrapeOnlineUsersAndHealth(c statsService.StatsServiceClient) error {
	ctx, cancel := context.WithTimeout(context.Background(), rpcTimeout)
	defer cancel()

	// 优化 Pattern，只查询用户相关 stats
	resp, err := c.QueryStats(ctx, &statsService.QueryStatsRequest{
		Pattern: "user>>>",
		Reset_:  false,
	})
	if err != nil {
		return err
	}

	// 🌟 Gauge 必须 Reset 以清除已下线的 IP 记录
	xrayUserIPOnline.Reset()

	// 提取用户列表
	users := make(map[string]struct{})
	for _, stat := range resp.Stat {
		user, ok := parseUser(stat.Name)
		if ok {
			users[user] = struct{}{}
		}
	}

	// 查询在线 IP 列表并设置 Gauge
	for user := range users {
		ctx2, cancel2 := context.WithTimeout(context.Background(), rpcTimeout)
		ipResp, err := c.GetStatsOnlineIpList(ctx2, &statsService.GetStatsRequest{
			Name: "user>>>" + user + ">>>online",
		})
		cancel2()
		if err != nil {
			log.Printf("GetStatsOnlineIpList error for user %s: %v", user, err)
			continue
		}

		for ip := range ipResp.Ips {
			xrayUserIPOnline.WithLabelValues(user, ip).Set(1) // 1 表示在线
		}
	}

	return nil
}

// ================= PARSERS =================

// parseUser 从统计名称中提取用户名
func parseUser(statName string) (string, bool) {
	parts := strings.Split(statName, ">>>")
	// 期望格式: user>>>username>>>traffic>>>up
	if len(parts) < 2 {
		return "", false
	}
	return parts[1], true
}
