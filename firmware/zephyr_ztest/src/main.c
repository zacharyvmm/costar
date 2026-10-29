/*
 * An unmodified ztest suite exercising Zephyr's kernel primitives: the ones
 * costar_legacy ran through its hand-built Zephyr kernel.  ztest ends the
 * run with the suite's result as the CPU's exit status.
 */
#include <zephyr/kernel.h>
#include <zephyr/ztest.h>

K_SEM_DEFINE(sem, 0, 1);
K_MUTEX_DEFINE(mutex);
K_MSGQ_DEFINE(msgq, sizeof(uint32_t), 4, 4);

static void give_after_sleep(void *a, void *b, void *c)
{
	k_msleep(5);
	k_sem_give(&sem);
}

K_THREAD_STACK_DEFINE(helper_stack, 4096);
static struct k_thread helper;

/* Zephyr rounds relative timeouts up by one tick (the current tick may be
 * partly over), so a 5 ms sleep at a tick boundary ends after 6 ms. */
ZTEST(kernel, test_sem_wakes_at_the_right_time)
{
	k_thread_create(&helper, helper_stack, K_THREAD_STACK_SIZEOF(helper_stack), give_after_sleep,
			NULL, NULL, NULL, K_PRIO_PREEMPT(1), 0, K_NO_WAIT);
	const int64_t start = k_uptime_get();

	zassert_ok(k_sem_take(&sem, K_MSEC(100)));
	zassert_equal(k_uptime_get() - start, 6);
	k_thread_join(&helper, K_FOREVER);
}

ZTEST(kernel, test_sem_times_out)
{
	const int64_t start = k_uptime_get();

	zassert_equal(k_sem_take(&sem, K_MSEC(7)), -EAGAIN);
	zassert_equal(k_uptime_get() - start, 8);
}

ZTEST(kernel, test_mutex_is_recursive)
{
	zassert_ok(k_mutex_lock(&mutex, K_NO_WAIT));
	zassert_ok(k_mutex_lock(&mutex, K_NO_WAIT));
	zassert_ok(k_mutex_unlock(&mutex));
	zassert_ok(k_mutex_unlock(&mutex));
}

ZTEST(kernel, test_msgq_keeps_order)
{
	for (uint32_t i = 1; i <= 4; i++) {
		zassert_ok(k_msgq_put(&msgq, &i, K_NO_WAIT));
	}
	zassert_equal(k_msgq_put(&msgq, &(uint32_t){5}, K_NO_WAIT), -ENOMSG);
	for (uint32_t i = 1; i <= 4; i++) {
		uint32_t got;

		zassert_ok(k_msgq_get(&msgq, &got, K_NO_WAIT));
		zassert_equal(got, i);
	}
}

static int work_runs;

static void work_handler(struct k_work *work)
{
	work_runs++;
}

ZTEST(kernel, test_delayable_work_runs_once_after_its_delay)
{
	static struct k_work_delayable work;

	k_work_init_delayable(&work, work_handler);
	k_work_schedule(&work, K_MSEC(3));
	k_msleep(2);
	zassert_equal(work_runs, 0);
	k_msleep(2);
	zassert_equal(work_runs, 1);
}

static int timer_expiries;

static void timer_expired(struct k_timer *timer)
{
	timer_expiries++;
}

ZTEST(kernel, test_periodic_timer)
{
	static struct k_timer timer;

	k_timer_init(&timer, timer_expired, NULL);
	k_timer_start(&timer, K_MSEC(4), K_MSEC(4));
	k_msleep(13);
	k_timer_stop(&timer);
	zassert_equal(timer_expiries, 3);
}

static void spin_forever(void *a, void *b, void *c)
{
	for (;;) {
		k_msleep(1);
	}
}

ZTEST(kernel, test_abort_another_thread)
{
	k_thread_create(&helper, helper_stack, K_THREAD_STACK_SIZEOF(helper_stack), spin_forever,
			NULL, NULL, NULL, K_PRIO_PREEMPT(1), 0, K_NO_WAIT);
	k_msleep(3);
	k_thread_abort(&helper);
	zassert_ok(k_thread_join(&helper, K_NO_WAIT));
}

ZTEST_SUITE(kernel, NULL, NULL, NULL, NULL, NULL);
