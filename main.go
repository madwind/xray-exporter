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
	xrayUserIPOnline = prometheus.NewGaugeVec(
		prometheus.GaugeOpts{
			Name: "xray_user_ip_online",
			Help: "User online status per IP (1=online)",
		},
		[]string{"name", "ip"},
	)

	xrayUp = prometheus.NewGauge(
		prometheus.GaugeOpts{
			Name: "xray_up",
			Help: "Whether Xray is reachable (1=up, 0=down)",
		},
	)
)

// ================= TRAFFIC COLLECTOR (CUSTOM) =================

type XrayTrafficCollector struct {
	client      statsService.StatsServiceClient
	trafficDesc *prometheus.Desc
}

func NewXrayTrafficCollector(client statsService.StatsServiceClient) *XrayTrafficCollector {
	return &XrayTrafficCollector{
		client: client,
		trafficDesc: prometheus.NewDesc(
			"xray_traffic_bytes_total",
			"Xray traffic statistics",
			[]string{"type", "name", "direction"},
			nil,
		),
	}
}

func (c *XrayTrafficCollector) Describe(ch chan<- *prometheus.Desc) {
	ch <- c.trafficDesc
}

func (c *XrayTrafficCollector) Collect(ch chan<- prometheus.Metric) {
	ctx, cancel := context.WithTimeout(context.Background(), rpcTimeout)
	defer cancel()

	resp, err := c.client.QueryStats(ctx, &statsService.QueryStatsRequest{
		Pattern: "",
		Reset_:  false,
	})
	if err != nil {
		log.Printf("TrafficCollector error during QueryStats: %v", err)
		return
	}

	for _, stat := range resp.Stat {
		if stat.Value == 0 {
			continue
		}
		if strings.Contains(stat.Name, ">>>traffic>>>") {
			parts := strings.Split(stat.Name, ">>>")
			if len(parts) < 4 {
				continue
			}
			typ := parts[0]
			nameLabel := parts[1]
			direction := parts[3]

			ch <- prometheus.MustNewConstMetric(
				c.trafficDesc,
				prometheus.CounterValue,
				float64(stat.Value),
				typ, nameLabel, direction,
			)
		}
	}
}

// ================= MAIN =================

func main() {
	log.Printf("Starting Xray exporter %s...\n", Version)

	conn, err := grpc.NewClient(AppConfig.XrayApi, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		log.Fatal("Connect to Xray failed:", err)
	}
	defer conn.Close()
	client := statsService.NewStatsServiceClient(conn)

	reg := prometheus.NewRegistry()

	trafficCollector := NewXrayTrafficCollector(client)
	reg.MustRegister(trafficCollector)

	reg.MustRegister(xrayUserIPOnline)
	reg.MustRegister(xrayUp)

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	go scrapeLoop(ctx, client)

	http.Handle("/metrics", promhttp.HandlerFor(reg, promhttp.HandlerOpts{}))
	addr := fmt.Sprintf(":%d", AppConfig.Port)
	log.Printf("Exporter listening on %s/metrics\n", addr)
	log.Fatal(http.ListenAndServe(addr, nil))
}

// ================= SCRAPE LOOP & FUNCTIONS =================

func scrapeLoop(ctx context.Context, client statsService.StatsServiceClient) {
	log.Println("Scrape loop started (single-thread mode)")

	failCount := 0

	time.Sleep(2 * time.Second)

	for {
		select {
		case <-ctx.Done():
			log.Println("Scrape loop stopped")
			return
		default:
			err := scrapeOnlineUsersAndHealth(client)
			if err != nil {
				failCount++
				xrayUp.Set(0)
				log.Println("scrapeOnlineUsersAndHealth error:", err)
			} else {
				failCount = 0
				xrayUp.Set(1)
			}

			sleep := scrapeInterval
			if failCount >= 3 {
				sleep = failInterval
			}
			time.Sleep(sleep)
		}
	}
}

func scrapeOnlineUsersAndHealth(c statsService.StatsServiceClient) error {
	ctx, cancel := context.WithTimeout(context.Background(), rpcTimeout)
	defer cancel()

	resp, err := c.QueryStats(ctx, &statsService.QueryStatsRequest{
		Pattern: "user>>>",
		Reset_:  false,
	})
	if err != nil {
		return err
	}

	xrayUserIPOnline.Reset()

	users := make(map[string]struct{})
	for _, stat := range resp.Stat {
		user, ok := parseUser(stat.Name)
		if ok {
			users[user] = struct{}{}
		}
	}

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

func parseUser(statName string) (string, bool) {
	parts := strings.Split(statName, ">>>")
	if len(parts) < 2 {
		return "", false
	}
	return parts[1], true
}
