import { QueryClient } from "@tanstack/react-query";

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      refetchOnWindowFocus: true,
      // 温和的默认缓存窗口：避免窗口反复聚焦时对未显式配置的查询发起重复请求。
      // 需要实时性的查询（代理状态、usage、failover、subscription 等）均已各自
      // 显式设置 staleTime / refetchInterval，会覆盖此默认值，不受影响。
      staleTime: 10 * 1000,
    },
    mutations: {
      retry: false,
    },
  },
});
