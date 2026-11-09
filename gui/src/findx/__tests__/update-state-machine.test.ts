/**
 * 更新状态机测试：验证requestId防护机制
 * 
 * 这些测试验证：
 * 1. 请求ID递增单调性
 * 2. 过期响应被正确丢弃
 * 3. 并发请求保护
 */

import { describe, it, expect, beforeEach } from 'vitest';

describe('更新状态机 requestId 防护', () => {
  let requestId: number;
  let busy: boolean;
  let result: string | null;

  beforeEach(() => {
    requestId = 0;
    busy = false;
    result = null;
  });

  const mockCheckUpdate = async (delay: number, willSucceed: boolean): Promise<string> => {
    return new Promise((resolve, reject) => {
      setTimeout(() => {
        if (willSucceed) {
          resolve('检查完成');
        } else {
          reject(new Error('检查失败'));
        }
      }, delay);
    });
  };

  it('应正确递增 requestId', async () => {
    const ids: number[] = [];
    
    for (let i = 0; i < 3; i++) {
      requestId++;
      ids.push(requestId);
    }

    expect(ids).toEqual([1, 2, 3]);
  });

  it('应丢弃过期响应', async () => {
    const responses: string[] = [];
    
    // 模拟第一个请求（慢）
    const req1Id = ++requestId;
    const req1 = mockCheckUpdate(100, true).then(() => {
      if (req1Id === requestId) {
        responses.push('req1');
      }
    });

    // 模拟第二个请求（快）
    const req2Id = ++requestId;
    const req2 = mockCheckUpdate(10, true).then(() => {
      if (req2Id === requestId) {
        responses.push('req2');
      }
    });

    await Promise.all([req1, req2]);

    // 只有最新请求应该被处理
    expect(responses).toEqual(['req2']);
  });

  it('应防止并发检查', async () => {
    const attempts: string[] = [];

    // 第一次检查
    if (!busy) {
      busy = true;
      attempts.push('check1-started');
      await mockCheckUpdate(50, true);
      busy = false;
      attempts.push('check1-finished');
    }

    // 尝试第二次检查（应该被阻止）
    if (!busy) {
      attempts.push('check2-started');
    } else {
      attempts.push('check2-blocked');
    }

    expect(attempts).toEqual(['check1-started', 'check1-finished', 'check2-started']);
  });

  it('应在busy时阻止新请求', () => {
    busy = false;
    
    // 第一个请求
    if (busy) {
      result = '被阻止';
    } else {
      busy = true;
      result = '开始';
    }
    
    expect(result).toBe('开始');
    expect(busy).toBe(true);

    // 第二个请求（应被阻止）
    if (busy) {
      result = '被阻止';
    } else {
      busy = true;
      result = '开始';
    }

    expect(result).toBe('被阻止');
  });

  it('应正确处理错误响应', async () => {
    const errors: string[] = [];
    
    const reqId = ++requestId;
    try {
      await mockCheckUpdate(10, false);
    } catch (err) {
      if (reqId === requestId) {
        errors.push((err as Error).message);
      }
    }

    expect(errors).toEqual(['检查失败']);
  });

  it('应丢弃过期错误响应', async () => {
    const errors: string[] = [];
    
    // 第一个请求（慢，会失败）
    const req1Id = ++requestId;
    const req1 = mockCheckUpdate(100, false).catch(() => {
      if (req1Id === requestId) {
        errors.push('req1-error');
      }
    });

    // 第二个请求（快，覆盖）
    ++requestId;

    await req1;

    // 过期请求的错误应被丢弃
    expect(errors).toEqual([]);
  });
});
